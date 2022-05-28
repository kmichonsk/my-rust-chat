use clap::Command;
use std::error::Error;

mod mio_tcp;

fn main() -> Result<(), Box<dyn Error>> {
    let _app = Command::new("chat-core");

    // wow, ugly
    mio_tcp::mio_tcp::run_chat()
}
