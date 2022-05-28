use mio::event::Event;
use mio::net::{TcpListener, TcpStream};
use mio::{Events, Interest, Poll, Token};

use std::error::Error;
use std::io::{Read, Write};
use std::time::Duration;

// ugly
use crate::mio_tcp::unique_token_generator::UniqueTokenGenerator;

pub fn run_chat() -> Result<(), Box<dyn Error>> {
    let mut chat = MioTcpChat::new()?;

    println!("You can connect to the server using `nc`:");
    println!(" $ nc 127.0.0.1 9000");

    chat.start_event_loop()
}

struct MioTcpChat {
    poll: Poll,
    token_generator: UniqueTokenGenerator,
    listener: TcpListener,
    listener_token: Token,
    chat_connection: Option<TcpStream>,
    chat_connection_token: Token,
}

impl MioTcpChat {
    /// Create and initialize tcp chat server
    fn new() -> Result<MioTcpChat, Box<dyn Error>> {
        let mut token_generator = UniqueTokenGenerator::new();
        let mut chat = MioTcpChat {
            poll: Poll::new()?,
            listener: TcpListener::bind("127.0.0.1:9000".parse()?)?,
            listener_token: token_generator.generate(),
            chat_connection: None,
            chat_connection_token: token_generator.generate(),
            token_generator,
        };

        // register our tcp listener for polling readable events from it
        // initially we just want to read messages
        chat.poll.registry().register(
            &mut chat.listener,
            chat.listener_token,
            Interest::READABLE,
        )?;

        Ok(chat)
    }

    fn start_event_loop(&mut self) -> Result<(), Box<dyn Error>> {
        // i don't like indent levels :)
        loop {
            self.event_loop_iteration()?;
        }
    }

    fn event_loop_iteration(&mut self) -> Result<(), Box<dyn Error>> {
        // Poll all registered event sources for events
        const TIMEOUT: Option<Duration> = Some(Duration::from_secs(16));
        let mut events_storage = Events::with_capacity(32);
        self.poll.poll(&mut events_storage, TIMEOUT)?;

        // Process each event we got
        for event in events_storage.iter() {
            println!("New event!\n\t{:?}\n", event);

            match event.token() {
                token if token == self.listener_token => self.handle_server_token_event()?,
                token if token == self.chat_connection_token => {
                    self.handle_chat_connection_token_event(event)?
                }
                _ => eprintln!("Weird event without associated connection: ${:?}", event),
            }
        }

        Ok(())
    }

    fn handle_server_token_event(&mut self) -> Result<(), Box<dyn Error>> {
        // why is a loop here tho?
        // is it that one event can mean multiple different connections?
        loop {
            // Received an event for the TCP server socket, which
            // indicates we can accept a new connection.
            let mut chat_connection = match self.listener.accept() {
                Ok((connection, address)) => (connection, address),
                Err(e) if Self::would_block(&e) => {
                    // No more queued connections, go back to polling
                    break;
                }
                Err(e) => return Err(Box::new(e)),
            };

            println!("Accepted connection from: {}", chat_connection.1);

            // Register this new connection
            // TODO: Can I register multiple clients on one connection
            //   what happens now?
            self.poll.registry().register(
                &mut chat_connection.0,
                self.chat_connection_token,
                Interest::READABLE,
            )?;

            self.chat_connection = Some(chat_connection.0);
        }
        Ok(())
    }

    fn handle_chat_connection_token_event(&mut self, event: &Event) -> Result<(), Box<dyn Error>> {
        // we're polling for readable events only
        if !event.is_readable() {
            unreachable!()
        }

        let chat_connection = self
            .chat_connection
            .as_mut()
            .expect("Early chat connection shutdown!");

        let mut received_data = vec![0; 4096];
        let mut bytes_read = 0;
        let mut connection_closed = false;

        loop {
            match chat_connection.read(&mut received_data[bytes_read..]) {
                Ok(0) => {
                    // Reading 0 bytes means the other side has closed the
                    // connection or is done writing, then so are we.
                    connection_closed = true;
                    break;
                }
                Ok(n) => {
                    bytes_read += n;
                    if bytes_read == received_data.len() {
                        received_data.resize(received_data.len() + 1024, 0);
                    }
                }
                // Would block "errors" are the OS's way of saying that the
                // connection is not actually ready to perform this I/O operation.
                Err(ref err) if Self::would_block(err) => break,
                Err(ref err) if Self::interrupted(err) => continue,
                Err(err) => return Err(Box::new(err)),
            }
        }

        if bytes_read != 0 {
            let received_data = &received_data[..bytes_read];
            if let Ok(str_buf) = std::str::from_utf8(received_data) {
                println!(
                    "{}: {}",
                    chat_connection.peer_addr().unwrap(),
                    str_buf.trim_end()
                );
            } else {
                println!(
                    "None UTF-8! {}: {:?}",
                    chat_connection.peer_addr().unwrap(),
                    received_data
                );
            }
        }

        if connection_closed {
            println!("Connection closed");
            self.poll.registry().deregister(chat_connection)?;
        }
        Ok(())
    }

    fn would_block(err: &std::io::Error) -> bool {
        err.kind() == std::io::ErrorKind::WouldBlock
    }

    fn interrupted(err: &std::io::Error) -> bool {
        err.kind() == std::io::ErrorKind::Interrupted
    }
}
