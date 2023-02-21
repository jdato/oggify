use std::env;
use env_logger::{Builder, Env};

mod reader;

fn main() {
    Builder::from_env(Env::default().default_filter_or("info")).init();

    let args: Vec<String> = env::args().collect();

    reader::read(args);
}
