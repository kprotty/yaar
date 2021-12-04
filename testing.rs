fn main() {
    uasync::Runtime::new(12).block_on(async move {
        use std::sync::{
            atomic::{AtomicUsize, Ordering::*},
            Once,
        };
        static O: Once = Once::new();
        static X: AtomicUsize = AtomicUsize::new(0);
        static Y: AtomicUsize = AtomicUsize::new(0);
        O.call_once(|| {
            std::thread::spawn(move || loop {
                std::thread::sleep(std::time::Duration::from_secs(1));
                println!("X:{} Y:{}", X.load(SeqCst), Y.load(SeqCst));
            });
        });

        let handles = (0..10_000_000)
            .map(|_| {
                uasync::Runtime::current().spawn(async move {
                    X.fetch_add(1, SeqCst);
                    // std::thread::yield_now();
                })
            })
            .collect::<Vec<_>>();

        for handle in handles {
            Y.fetch_add(1, SeqCst);
            handle.await;
        }
    });
}
