use mio::event::Event;
use mio::net::{TcpListener, TcpStream};
use mio::{Events, Interest, Poll, Token};
use std::collections::HashMap;

use std::error::Error;
use std::io::Read;
use std::time::Duration;
use log::{debug, info, warn};

use crate::mio_tcp::utils::UniqueTokenGenerator;

pub fn run_chat() -> Result<(), Box<dyn Error>> {
    let mut chat = MioTcpChat::new()?;

    info!("You can connect to the server using `nc`:");
    info!(" $ nc 127.0.0.1 9000");

    chat.start_event_loop()
}

struct MioTcpChat {
    poll: Poll,
    token_generator: UniqueTokenGenerator,
    listener: TcpListener,
    listener_token: Token,
    client_connections: HashMap<Token, TcpStream>,
}

impl MioTcpChat {
    /// Create and initialize the tcp chat server struct
    fn new() -> Result<MioTcpChat, Box<dyn Error>> {
        let mut token_generator = UniqueTokenGenerator::new();
        let mut chat = MioTcpChat {
            poll: Poll::new()?,
            listener: TcpListener::bind("127.0.0.1:9000".parse()?)?,
            listener_token: token_generator.generate(),
            client_connections: HashMap::new(),
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
            debug!("New event!\n\t{:?}\n", event);

            match event.token() {
                token if token == self.listener_token => self.handle_server_token_event()?,
                _ => self.handle_client_connection_token_event(event)?,
            }
        }

        Ok(())
    }

    fn handle_server_token_event(&mut self) -> Result<(), Box<dyn Error>> {
        // why is a loop here tho?
        // is it that one event can mean multiple different connections?
        loop {
            let (mut client_connection, address) = match self.listener.accept() {
                Ok((connection, address)) => (connection, address),
                Err(e) if Self::would_block(&e) => {
                    // No more queued connections, go back to polling
                    break;
                }
                Err(e) => return Err(Box::new(e)),
            };

            info!("Accepted connection from: {}", address);

            let client_connection_token = self.token_generator.generate();
            self.poll.registry().register(
                &mut client_connection,
                client_connection_token,
                Interest::READABLE,
            )?;

            self.client_connections.insert(client_connection_token, client_connection);
        }
        Ok(())
    }

    fn handle_client_connection_token_event(&mut self, event: &Event) -> Result<(), Box<dyn Error>> {
        // first verify if we know of this connection
        let client_connection = match self.client_connections.get_mut(&event.token()) {
            Some(connection ) => connection,
            None => {
                warn!("Weird event without associated connection: ${:?}", event);
                return Ok(());
            }
        };

        let mut received_data = vec![0; 4096];
        let mut bytes_read = 0;
        let mut connection_closed = false;

        loop {
            // write starting from where we left off
            match client_connection.read(&mut received_data[bytes_read..]) {
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
                Err(ref err) if Self::would_block(err) => break,
                Err(ref err) if Self::interrupted(err) => continue,
                Err(err) => return Err(Box::new(err)),
            }
        }

        if bytes_read != 0 {
            let received_data = &received_data[..bytes_read];
            let peer_addr = client_connection
                .peer_addr()
                .unwrap_or_else(|_| "0.0.0.0".parse().unwrap());
            if let Ok(str_buf) = std::str::from_utf8(received_data) {
                info!("{}: {}", peer_addr, str_buf.trim_end());
            } else {
                warn!("None UTF-8 data from {}", peer_addr);
            }
        }

        if connection_closed {
            info!("Connection closed");
            self.poll.registry().deregister(client_connection)?;
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
