use clap::Command;
use std::error::Error;

mod mio_tcp;

fn main() -> Result<(), Box<dyn Error>> {
    let _app = Command::new("chat-core");

    mio_tcp::chat::run_chat()
}
