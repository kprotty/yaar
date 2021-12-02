#![forbid(unsafe_code)]

#[allow(unused)]
mod dependencies;

#[cfg(feature = "io")]
pub use tokio::io;

#[cfg(feature = "rt")]
pub mod runtime;

#[cfg(test)]
mod test {
    #[test]
    fn hello_world() {
        crate::runtime::block_on(async move {
            static O: std::sync::Once = std::sync::Once::new();
            static X: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            static Y: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            O.call_once(|| {
                let _ = std::thread::spawn(move || loop {
                    std::thread::sleep(std::time::Duration::from_secs(1));
                    println!("X:{} Y:{}", X.load(std::sync::atomic::Ordering::SeqCst), Y.load(std::sync::atomic::Ordering::SeqCst));
                });
            });

            let started = std::time::Instant::now();
            let handles = (0..10_000_000).map(|_i| {
                crate::runtime::spawn(async move {
                    std::thread::yield_now();
                    X.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                })
            });

            let x = handles.collect::<Vec<_>>();
            for handle in x.into_iter() {
                handle.await;
                Y.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
            println!("took {:?}", started.elapsed());
        })
    }
}
