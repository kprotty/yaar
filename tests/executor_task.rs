use core::{pin::Pin, ptr::NonNull};
use yaar::executor::{Task, TaskBatch, TaskCallback, Thread};

#[test]
fn task_run() {
    #[repr(C)]
    struct TaskContext {
        task: Task,
        is_set: bool,
    }

    // have a callback which sets the context
    extern "C" fn set_ctx(task: *mut Task, _: *const Thread) {
        unsafe { (*(task as *mut TaskContext)).is_set = true };
    }

    // create an unset context
    let mut ctx = TaskContext {
        task: Task::from(set_ctx as TaskCallback),
        is_set: false,
    };

    // call the callback which shuld set the context
    unsafe {
        let task = Pin::new_unchecked(&mut ctx.task);
        task.run(Pin::new_unchecked(&Thread::default()));
    }

    assert!(ctx.is_set);
}

#[test]
fn task_batch() {
    extern "C" fn stub_fn(_: *mut Task, _: *const Thread) {}

    let (mut t1, mut t2, mut t3) = (
        Task::from(stub_fn as TaskCallback),
        Task::from(stub_fn as TaskCallback),
        Task::from(stub_fn as TaskCallback),
    );

    unsafe {
        let mut batch = TaskBatch::new();
        assert_eq!(batch.pop(), None);

        // Test basic push and pop

        batch.push(Pin::new_unchecked(&mut t1));
        batch.push(Pin::new_unchecked(&mut t2));
        assert_eq!(batch.pop(), NonNull::new(&mut t1));

        batch.push(Pin::new_unchecked(&mut t3));
        assert_eq!(batch.pop(), NonNull::new(&mut t2));
        assert_eq!(batch.pop(), NonNull::new(&mut t3));
        assert_eq!(batch.pop(), None);

        // Test push from different sides of the batch

        batch.push(Pin::new_unchecked(&mut t1));
        batch.push_front(Pin::new_unchecked(&mut t2));
        batch.push_back(Pin::new_unchecked(&mut t3));

        assert_eq!(batch.pop(), NonNull::new(&mut t2));
        assert_eq!(batch.pop(), NonNull::new(&mut t1));
        assert_eq!(batch.pop(), NonNull::new(&mut t3));
        assert_eq!(batch.pop(), None);

        // Test different side push + pushing of entire batches

        batch.push({
            let mut b = TaskBatch::new();
            b.push(Pin::new_unchecked(&mut t2));
            b.push(Pin::new_unchecked(&mut t3));
            b
        });
        batch.push_front(Pin::new_unchecked(&mut t1));

        assert_eq!(batch.pop(), NonNull::new(&mut t1));
        assert_eq!(batch.pop(), NonNull::new(&mut t2));
        assert_eq!(batch.pop(), NonNull::new(&mut t3));
        assert_eq!(batch.pop(), None);

        // Test batch iteration

        batch.push(Pin::new_unchecked(&mut t1));
        batch.push(Pin::new_unchecked(&mut t2));
        batch.push(Pin::new_unchecked(&mut t3));

        {
            let mut tasks = batch.iter();
            assert_eq!(tasks.next(), NonNull::new(&mut t1));
            assert_eq!(tasks.next(), NonNull::new(&mut t2));
            assert_eq!(tasks.next(), NonNull::new(&mut t3));
            assert_eq!(tasks.next(), None);
        }

        assert_eq!(batch.pop(), NonNull::new(&mut t1));
        assert_eq!(batch.pop(), NonNull::new(&mut t2));
        assert_eq!(batch.pop(), NonNull::new(&mut t3));
        assert_eq!(batch.pop(), None);
    }
}
