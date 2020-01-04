use zap::runtime::{
    config::Config,
    handler::DefaultHandler,
    platform::{OsPlatform, StdAllocator},
};

fn main() {
    Config
        ::new(
            OsPlatform::default(),
            StdAllocator::default(),
            DefaultHandler::default(),
        )
        .run(async {
            println!("Hello world");
            42usize
        })
        .map(|x| assert_eq!(x, 42))
        .unwrap();
}