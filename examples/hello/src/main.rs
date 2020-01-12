use yaar::runtime::{serial, task};

fn main() {
    let result = serial::run(async move {
        for i in 0..5 {
            println!("Hello world {}", i);
            task::yield_now(task::Priority::Normal).await;
        }
        42usize
    });
    assert_eq!(result, 42);
}
