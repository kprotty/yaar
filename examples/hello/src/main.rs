
fn main() {
    let result = yaar::runtime::serial::run(
        async move {
            println!("Hello world");
            42usize
        },
    );
    assert_eq!(result, 42);
}