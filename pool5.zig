const std = @import("std");
const assert = std.debug.assert;
const Atomic = std.atomic.Atomic;

const ThreadPool = @This();

const arch = std.Target.current.cpu.arch;
const cache_line_padding = switch (arch) {
    .riscv64, .arm, .armeb, .thumb, .thumbeb, .mips, .mipsel, .mips64, .mips64el => 32,
    .x86_64, .powerpc64, .powerpc64le => 128,
    .s390x => 256,
    else => 64,
};

pub const ThreadCount = Sync.Count;
pub const Config = struct {
    max_threads: ThreadCount,
    stack_size: usize = (std.Thread.SpawnConfig{}).stack_size,
};

config: Config,
join_event: Event = .{},
workers: Atomic(?*Worker) = Atomic(?*Worker).init(null),
runnable: List align(cache_line_padding) = .{},
idle_event: Event align(cache_line_padding) = .{},
sync: Atomic(usize) align(cache_line_padding) = Atomic(usize).init(@bitCast(usize, Sync{})),

pub fn init(config: Config) ThreadPool {
    return .{ .config = config };
}

pub fn deinit(self: *ThreadPool) void {
    self.join();
    self.* = undefined;
}

pub const Runnable = struct {
    next: ?*Runnable = null,
    runFn: fn(*Runnable) void,

    pub fn init(runFn: fn(*Runnable) void) Runnable {
        return .{ .runFn = runFn };
    }
};

pub const Batch = struct {
    head: ?*Runnable = null,
    tail: ?*Runnable = null,

    pub fn from(runnable: *Runnable) Batch {
        runnable.next = null;
        return Batch{
            .head = runnable,
            .tail = runnable,
        };
    }

    pub fn isEmpty(self: Batch) bool {
        return self.head == null;
    }

    pub fn push(self: *Batch, batch: Batch) void {
        if (batch.head == null) return;
        if (self.isEmpty()) self.tail = batch.tail;
        batch.tail.?.next = self.head;
        self.head = batch.head;
    }

    pub fn pop(self: *Batch) ?*Runnable {
        const runnable = self.head orelse return null;
        self.head = runnable.next;
        return runnable;
    }
};

pub fn schedule(self: *ThreadPool, batch: Batch) void {
    if (batch.isEmpty()) {
        return;
    }

    if (Worker.current) |worker| {
        worker.push(batch);
    } else {
        self.runnable.push(batch);
    }

    const sync = @bitCast(Sync, self.sync.load(.Monotonic));
    if (sync.notified) {
        return;
    }

    const is_waking = false;
    self.notify(is_waking);
}

const Sync = packed struct {
    const Count = std.meta.Int(
        .unsigned,
        (std.meta.bitCount(usize) - 3) / 2,
    );

    const StateTag = std.meta.Int(
        .unsigned,
        std.meta.bitCount(usize) - 1 - (std.meta.bitCount(Count) * 2),
    );

    comptime {
        assert(@bitSizeOf(Sync) == @bitSizeOf(usize));
    }

    idle: Count = 0,
    spawned: Count = 0,
    notified: bool = false,
    state: enum(StateTag) {
        pending = 0,
        waking,
        signaled,
        shutdown,
    } = .pending,
};

fn notify(self: *ThreadPool, is_waking: bool) void {
    @setCold(true);

    const max_spawn = self.config.max_threads;
    var sync = @bitCast(Sync, self.sync.load(.Monotonic));

    while (true) {
        if (sync.state == .shutdown) return;
        if (is_waking) assert(sync.state == .waking);
        const can_wake = is_waking or (sync.state == .pending);

        var new_sync = sync;
        new_sync.notified = true;
        if (sync.idle > 0 and can_wake) {
            new_sync.state = .signaled;
        } else if (sync.spawned < max_spawn and can_wake) {
            new_sync.state = .signaled;
            new_sync.spawned += 1;
        } else if (is_waking) {
            new_sync.state = .pending;
        } else if (sync.notified) {
            return;
        }

        if (self.sync.tryCompareAndSwap(
            @bitCast(usize, sync),
            @bitCast(usize, new_sync),
            .Release,
            .Monotonic,
        )) |updated| {
            sync = @bitCast(Sync, updated);
            continue;
        }

        if (sync.idle > 0 and can_wake) {
            self.idle_event.notify();
        } else if (sync.spawned < max_spawn and can_wake) {
            Worker.spawn(self) catch self.unregister(null);
        }

        return;
    }
}

fn wait(self: *ThreadPool, _is_waking: bool) error{Shutdown}!bool {
    @setCold(true);

    var is_idle = false;
    var is_waking = _is_waking;
    var sync = @bitCast(Sync, self.sync.load(.Monotonic));

    while (true) {
        if (sync.state == .shutdown) return error.Shutdown;
        if (is_waking) assert(sync.state == .waking);

        if (sync.notified) {
            var new_sync = sync;
            new_sync.notified = false;
            if (is_idle) new_sync.idle -= 1;
            if (sync.state == .signaled) new_sync.state = .waking;

            if (self.sync.tryCompareAndSwap(
                @bitCast(usize, sync),
                @bitCast(usize, new_sync),
                .Acquire,
                .Monotonic,
            )) |updated| {
                sync = @bitCast(Sync, updated);
                continue;
            }

            is_waking = is_waking or sync.state == .signaled;
            return is_waking;
        }

        if (!is_idle) {
            var new_sync = sync;
            new_sync.idle += 1;
            if (is_waking) new_sync.state = .pending;

            if (self.sync.tryCompareAndSwap(
                @bitCast(usize, sync),
                @bitCast(usize, new_sync),
                .Monotonic,
                .Monotonic,
            )) |updated| {
                sync = @bitCast(Sync, updated);
                continue;
            }

            is_idle = true;
            is_waking = false;
        }

        self.idle_event.wait();
        sync = @bitCast(Sync, self.sync.load(.Monotonic));
    }
}

pub fn shutdown(self: *ThreadPool) void {
    @setCold(true);

    var sync = @bitCast(Sync, self.sync.load(.Monotonic));
    while (sync.state != .shutdown) {
        var new_sync = sync;
        new_sync.idle = 0;
        new_sync.notified = true;
        new_sync.state = .shutdown;

        if (self.sync.tryCompareAndSwap(
            @bitCast(usize, sync),
            @bitCast(usize, new_sync),
            .AcqRel,
            .Monotonic,
        )) |updated| {
            sync = @bitCast(Sync, updated);
            continue;
        }

        if (sync.idle > 0) {
            self.idle_event.shutdown();
        }

        return;
    }
}

fn register(noalias self: *ThreadPool, noalias worker: *Worker) void {
    @setCold(true);

    var workers = self.workers.load(.Monotonic);
    while (true) {
        worker.next = workers;
        workers = self.workers.tryCompareAndSwap(
            workers,
            worker,
            .Release,
            .Monotonic,
        ) orelse break;
    }
}

fn unregister(noalias self: *ThreadPool, noalias maybe_worker: ?*Worker) void {
    @setCold(true);
    
    // Remove a spawned worker from the sync state
    const remove = @bitCast(usize, Sync{ .spawned = 1 });
    const updated = self.sync.fetchSub(remove, .AcqRel);
    const sync = @bitCast(Sync, updated);

    assert(sync.state == .shutdown);
    assert(sync.spawned >= 1);
    
    // Notify the join() threads waiting for all workers to be unregistered/joinable
    if (sync.spawned == 1) {
        self.join_event.notify();
    }

    // If unregistering a worker, wait for a shutdown signal
    const worker = maybe_worker orelse return;
    worker.join_event.wait();

    // After receiving a shutdown signal, shutdown the next worker it's linked with
    const next_worker = worker.next orelse return;
    next_worker.join_event.notify();
}

fn join(self: *ThreadPool) void {
    @setCold(true);

    // Wait for the thread pool to be shutdown and for all workers to be unregistered.
    const sync = @bitCast(Sync, self.sync.load(.Monotonic));
    if (!(sync.state == .shutdown and sync.spawned == 0)) {
        self.join_event.wait();
    }

    // Shutdown the top-most worker, which will shutdown the next worker, and so on...
    const worker = self.workers.load(.Acquire) orelse return;
    worker.join_event.notify();
}

const Worker = struct {
    next: ?*Worker = undefined,
    join_event: Event = .{},
    runnable: List align(cache_line_padding) = .{},
    buffer: Buffer align(cache_line_padding) = .{},

    threadlocal var current: ?*Worker = null;

    fn spawn(thread_pool: *ThreadPool) !void {
        const thread = try std.Thread.spawn(
            .{ .stack_size = thread_pool.config.stack_size },
            Worker.run,
            .{ thread_pool },
        );
        thread.detach();
    }

    fn run(thread_pool: *ThreadPool) void {
        var self = Worker{};
        current = &self;

        thread_pool.register(&self);
        defer thread_pool.unregister(&self);
        
        var is_waking = false;
        var xorshift = @truncate(u32, @ptrToInt(&self) *% 31) | 1;

        while (true) {
            is_waking = thread_pool.wait(is_waking) catch break;

            while (self.pop(thread_pool, &xorshift)) |popped| {
                if (popped.pushed or is_waking) {
                    thread_pool.notify(is_waking);
                }

                is_waking = false;
                (popped.runnable.runFn)(popped.runnable);
            }
        }
    }

    fn push(self: *Worker, batch: Batch) void {
        if (self.buffer.push(batch)) |overflowed| {
            self.runnable.push(overflowed);
        }
    }

    fn pop(
        noalias self: *Worker,
        noalias thread_pool: *ThreadPool,
        noalias xorshift: *u32,
    ) callconv(.Inline) ?Buffer.Popped {
        if (self.buffer.pop()) |popped| {
            return popped;
        }

        return self.popAndSteal(thread_pool, xorshift);
    }

    fn popAndSteal(
        noalias self: *Worker, 
        noalias thread_pool: *ThreadPool,
        noalias xorshift: *u32,
    ) ?Buffer.Popped {
        @setCold(true);

        if (self.buffer.popAndSteal(&self.runnable)) |popped| {
            return popped;
        } else |_| {}

        if (self.buffer.popAndSteal(&thread_pool.runnable)) |popped| {
            return popped;
        } else |_| {}

        var attempts: u8 = 4;
        while (attempts > 0) : (attempts -= 1) {
            return self.tryPopAndSteal(thread_pool, xorshift) catch {
                std.atomic.spinLoopHint();
                continue;
            };
        }

        if (self.buffer.popAndSteal(&thread_pool.runnable)) |popped| {
            return popped;
        } else |_| {}

        return null;
    }

    fn tryPopAndSteal(
        noalias self: *Worker,
        noalias thread_pool: *ThreadPool,
        noalias xorshift: *u32,
    ) error{Empty, Contended}!Buffer.Popped {
        var rng = xorshift.*;
        rng ^= rng >> 13;
        rng ^= rng << 17;
        rng ^= rng >> 5;
        xorshift.* = rng;

        var index: u8 = 0;
        var was_contended = false;
        var buffer = std.mem.zeroes([8]?*Worker);
        var workers = thread_pool.workers.load(.Acquire);

        while (true) {
            const target: ?*Worker = blk: {
                while (true) {
                    while (index < buffer.len) {
                        const worker = buffer[index];
                        index += 1;
                        break :blk (worker orelse continue);
                    }

                    if (workers == null) {
                        break :blk null;
                    }

                    index = 0;
                    for (buffer) |*slot| {
                        slot.* = workers;
                        const worker = slot.* orelse continue;
                        workers = worker.next;
                    }

                    var i = buffer.len - 1;
                    while (i > 0) : (i -= 1) {
                        const j = rng % (i + 1);
                        std.mem.swap(?*Worker, &buffer[i], &buffer[j]);
                    }
                }
            };
            
            const target_worker = target orelse break;
            if (self.buffer.popAndSteal(&target_worker.runnable)) |popped| {
                return popped;
            } else |err| {
                was_contended = was_contended or err == error.Contended;
            }

            if (target_worker == self) {
                continue;
            }

            return target_worker.buffer.steal() catch |err| {
                was_contended = was_contended or err == error.Contended;
                continue;
            };
        }

        if (was_contended) return error.Contended;
        return error.Empty;
    }
};

const Buffer = struct {
    head: Atomic(Index) align(cache_line_padding) = Atomic(Index).init(0),
    tail: Atomic(Index) align(cache_line_padding) = Atomic(Index).init(0),
    array: [capacity]Atomic(*Runnable) = undefined,

    const Index = std.meta.Int(.unsigned, std.meta.bitCount(usize) / 2);
    const capacity = 256;
    comptime {
        assert(std.math.maxInt(Index) >= capacity);
    }

    fn push(self: *Buffer, batch: Batch) ?Batch {
        assert(!batch.isEmpty());

        var runnables = batch;
        var tail = self.tail.loadUnchecked();
        var head = self.head.load(.Monotonic);

        while (true) {
            const size = tail -% head;
            assert(size <= capacity);

            var free_slots = capacity - size;
            if (free_slots > 0) {
                while (free_slots > 0) : (free_slots -= 1) {
                    const runnable = runnables.pop() orelse break;
                    runnable.next = self.array[(tail -% 1) % capacity].loadUnchecked();
                    self.array[tail % capacity].store(runnable, .Unordered);
                    tail +%= 1;
                }

                self.tail.store(tail, .Release);
                if (runnables.isEmpty()) {
                    return null;
                }
                
                std.atomic.spinLoopHint();
                head = self.head.load(.Monotonic);
                continue;
            }

            var overflow: Index = capacity / 2;
            if (self.head.tryCompareAndSwap(
                head,
                head +% overflow,
                .Acquire,
                .Monotonic,
            )) |updated| {
                head = updated;
                continue;
            }

            const front = self.array[(head +% (overflow - 1)) % capacity].loadUnchecked();
            const back = self.array[head % capacity].loadUnchecked();
            back.next = null;
            
            runnables.push(Batch {
                .head = front,
                .tail = back,
            });

            return runnables;
        }
    }

    const Popped = struct {
        runnable: *Runnable,
        pushed: bool = false,
    };

    fn pop(self: *Buffer) ?Popped {
        const tail = self.tail.loadUnchecked();
        var head = self.head.load(.Monotonic);
        
        assert(tail -% head <= capacity);
        if (tail == head) {
            return null;
        }

        const new_tail = tail -% 1;
        self.tail.store(new_tail, .SeqCst);
        head = self.head.load(.SeqCst);

        const size = tail -% head;
        assert(size <= capacity);

        const runnable = self.array[new_tail % capacity].loadUnchecked();
        if (size > 1) {
            return Popped{ .runnable = runnable };
        }

        self.tail.store(tail, .Monotonic);
        if (size == 1) {
            _ = self.head.compareAndSwap(
                head,
                tail,
                .Acquire,
                .Monotonic,
            ) orelse return Popped{ .runnable = runnable };
        }

        return null;
    }

    fn steal(self: *Buffer) error{Empty, Contended}!Popped {
        const head = self.head.load(.Acquire);
        const tail = self.tail.load(.Acquire);

        const size = tail -% head;
        if (size == 0 or size > capacity) {
            return error.Empty;
        }
        
        const runnable = self.array[head % capacity].load(.Unordered);
        _ = self.head.compareAndSwap(
            head,
            head +% 1,
            .SeqCst,
            .Monotonic,
        ) orelse return Popped{ .runnable = runnable };

        return error.Contended;
    }

    fn popAndSteal(noalias self: *Buffer, noalias target: *List) error{Empty, Contended}!Popped {
        var consumer = target.tryAcquireConsumer() catch return error.Empty;
        defer consumer.release(); 

        const tail = self.tail.loadUnchecked();
        const head = self.head.load(.Monotonic);

        const size = tail -% head;
        assert(size == 0);

        const first = consumer.pop();
        var pushed: Index = 0;
        while (pushed < capacity) : (pushed += 1) {
            const runnable = consumer.pop() orelse break;
            runnable.next = self.array[((tail +% pushed) -% 1) % capacity].loadUnchecked();
            self.array[(tail +% pushed) % capacity].store(runnable, .Unordered);
        }

        const popped = first orelse consumer.pop() orelse blk: {
            if (pushed == 0) return error.Empty;
            pushed -= 1;
            break :blk self.array[(tail +% pushed) % capacity].loadUnchecked();
        };

        if (pushed > 0) self.tail.store(tail +% pushed, .Release);
        return Popped {
            .runnable = popped,
            .pushed = pushed > 0,
        };
    }
};

const List = struct {
    stack: Atomic(usize) = Atomic(usize).init(0),
    local: ?*Runnable align(cache_line_padding) = null,

    const HAS_LOCAL: usize = 1 << 1;
    const HAS_CONSUMER: usize = 1 << 0;

    const PTR_MASK = ~(HAS_CONSUMER | HAS_LOCAL);
    comptime {
        assert(@alignOf(Runnable) >= ~PTR_MASK + 1);
    }

    fn push(self: *List, batch: Batch) void {
        assert(!batch.isEmpty());

        const head = batch.head orelse unreachable;
        const tail = batch.tail orelse unreachable;

        var stack = self.stack.load(.Monotonic);
        while (true) {
            tail.next = @intToPtr(?*Runnable, stack & PTR_MASK);

            stack = self.stack.tryCompareAndSwap(
                stack,
                @ptrToInt(head) | (stack & ~PTR_MASK),
                .Release,
                .Monotonic,
            ) orelse break;
        }
    }

    fn tryAcquireConsumer(self: *List) error{Empty, Contended}!Consumer {
        var stack = self.stack.load(.Monotonic);
        while (true) {
            // Return if there's no pushed pointer or HAS_LOCAL
            if (stack & ~HAS_CONSUMER == 0)
                return error.Empty;

            // Return if there's already a consumer
            if (stack & HAS_CONSUMER != 0)
                return error.Contended;

            // Mark that the stack is being consumed and will have a local stack
            // Also claim the stack of Runnables if there is no self.local
            var new_stack = stack | HAS_CONSUMER | HAS_LOCAL;
            if (stack & HAS_LOCAL == 0) {
                new_stack &= ~PTR_MASK;
            }

            stack = self.stack.tryCompareAndSwap(
                stack,
                new_stack,
                .Acquire,
                .Monotonic,
            ) orelse return Consumer{
                .list = self,
                .local = self.local orelse @intToPtr(?*Runnable, stack & PTR_MASK),
            };
        }
    }

    const Consumer = struct {
        list: *List,
        local: ?*Runnable,

        fn pop(self: *Consumer) ?*Runnable {
            const runnable = self.local orelse return self.take();
            self.local = runnable.next;
            return runnable;
        }

        fn take(self: *Consumer) ?*Runnable {
            @setCold(true);

            var stack = self.list.stack.load(.Monotonic);
            if (stack & PTR_MASK != 0) {
                stack = self.list.stack.swap(HAS_LOCAL | HAS_CONSUMER, .Acquire);
            }

            const runnable = @intToPtr(?*Runnable, stack & PTR_MASK) orelse return null;
            self.local = runnable.next;
            return runnable;
        }

        fn release(self: Consumer) void {
            var remove: usize = HAS_CONSUMER;
            if (self.local == null) {
                remove |= HAS_LOCAL;
            }

            self.list.local = self.local;
            _ = self.list.stack.fetchSub(remove, .Release);
        }
    };
};

const Event = struct {
    state: Atomic(State) = Atomic(State).init(.empty),

    const Futex = std.Thread.Futex;
    const State = enum(u32) {
        empty = 0,
        waiting,
        notified,
        shutdown,
    };

    fn wait(self: *Event) void {
        @setCold(true);

        var wake_state = State.empty;
        var state = self.state.load(.Monotonic);
        while (true) {
            switch (state) {
                .empty => {
                    state = self.state.tryCompareAndSwap(
                        state,
                        .waiting,
                        .Monotonic,
                        .Monotonic,
                    ) orelse .waiting;
                },
                .waiting => {
                    Futex.wait(
                        @ptrCast(*const Atomic(u32), &self.state),
                        @enumToInt(State.waiting),
                        null,
                    ) catch unreachable;
                    wake_state = .waiting;
                    state = self.state.load(.Monotonic);
                },
                .notified => {
                    state = self.state.tryCompareAndSwap(
                        state,
                        wake_state,
                        .Acquire,
                        .Monotonic,
                    ) orelse return;
                },
                .shutdown => {
                    return;
                },
            }
        }
    }

    fn notify(self: *Event) void {
        @setCold(true);

        var state = self.state.load(.Monotonic);
        while (true) {
            switch (state) {
                .empty => {},
                .waiting => {},
                .notified => return,
                .shutdown => return,
            }

            state = self.state.tryCompareAndSwap(
                state,
                .notified,
                .Release,
                .Monotonic,
            ) orelse return Futex.wake(
                @ptrCast(*const Atomic(u32), &self.state),
                @as(u32, 1),
            );
        }
    }

    fn shutdown(self: *Event) void {
        @setCold(true);

        self.state.store(.shutdown, .Release);
        Futex.wake(
            @ptrCast(*const Atomic(u32), &self.state),
            std.math.maxInt(u32),
        );
    }
};