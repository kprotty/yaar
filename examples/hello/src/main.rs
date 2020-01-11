use yaar::runtime::{task, serial};

fn main() {
    let result = serial::run(
        async move {
            for i in 0..5 {
                println!("Hello world {}", i);
                task::yield_now().await;
            }
            42usize
        },
    );
    assert_eq!(result, 42);
}