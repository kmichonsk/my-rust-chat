use mio::event::Event;
use mio::net::{TcpListener, TcpStream};
use mio::{Events, Interest, Poll, Token};
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};

use log::{debug, info, warn};
use std::error::Error;
use std::io::{Read, Write};
use std::net::SocketAddr;
use std::rc::Rc;
use std::time::Duration;

use crate::mio_tcp::utils::UniqueTokenGenerator;

pub fn run_chat() -> Result<(), Box<dyn Error>> {
    let mut chat = MioTcpChat::new()?;

    info!("You can connect to the server using `nc`:");
    info!(" $ nc 127.0.0.1 9000");

    chat.start_event_loop()
}

const TIMEOUT: Option<Duration> = Some(Duration::from_secs(16));
const READ_BUFFER_SIZE: usize = 4096;

#[derive(Debug)]
struct Message {
    from: SocketAddr,
    content: String,
}

impl std::fmt::Display for Message {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "({}: {})", self.from.to_string(), self.content)
    }
}

// TODO: add peer_addr, with tpc_streams lifetime to the ClientConnection struct,
//   checking every time for valid socketaddr from tcpstream is annoying
struct ClientConnection {
    address: SocketAddr,
    tcp_stream: TcpStream,
    pending_messages: VecDeque<Rc<RefCell<Message>>>,
}

impl ClientConnection {
    fn new(address: SocketAddr, tcp_stream: TcpStream) -> ClientConnection {
        ClientConnection {
            pending_messages: VecDeque::new(),
            address,
            tcp_stream,
        }
    }
}

struct MioTcpChat {
    poll: Poll,
    token_generator: UniqueTokenGenerator,
    listener: TcpListener,
    listener_token: Token,
    client_connections: HashMap<Token, ClientConnection>,
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

        let mut events_storage = Events::with_capacity(32);
        self.poll.poll(&mut events_storage, TIMEOUT)?;

        for event in events_storage.iter() {
            debug!("New event:\n\t{:?}\n", event);

            match event.token() {
                token if token == self.listener_token => self.handle_server_token_event()?,
                _ => self.handle_client_connection_token_event(event)?,
            }
        }

        Ok(())
    }

    fn handle_server_token_event(&mut self) -> Result<(), Box<dyn Error>> {
        loop {
            let (mut client_connection, address) = match self.listener.accept() {
                Ok((connection, address)) => (connection, address),
                // No more queued connections, go back to polling
                Err(e) if Self::would_block(&e) => break,
                Err(e) => return Err(Box::new(e)),
            };

            info!("Accepted connection from: {}", address);

            let client_connection_token = self.token_generator.generate();
            self.poll.registry().register(
                &mut client_connection,
                client_connection_token,
                Interest::READABLE.add(Interest::WRITABLE),
            )?;

            self.client_connections.insert(
                client_connection_token,
                ClientConnection::new(address, client_connection),
            );
        }
        Ok(())
    }

    fn handle_client_connection_token_event(
        &mut self,
        event: &Event,
    ) -> Result<(), Box<dyn Error>> {
        let token = &event.token();
        if !self.client_connections.contains_key(token) {
            warn!("Weird event without associated connection: ${:?}", event);
            return Ok(());
        }

        if event.is_readable() {
            let mut received_data = vec![0; READ_BUFFER_SIZE];
            let (did_close, bytes_read) = self.read_from_client(token, &mut received_data)?;
            if bytes_read != 0 {
                self.handle_client_message(token, &mut received_data, bytes_read);
            }

            if did_close {
                let mut client_connection = self.client_connections.remove(token).unwrap();
                info!("Connection with {} closed", &client_connection.address);
                self.poll
                    .registry()
                    .deregister(&mut client_connection.tcp_stream)?;
                return Ok(());
            }
        }

        if event.is_writable() {
            self.write_pending_messages_to_client(token)?;
        }

        Ok(())
    }

    /// returns (did_connection_close, bytes_read)
    fn read_from_client(
        &mut self,
        token: &Token,
        received_data: &mut Vec<u8>,
    ) -> Result<(bool, usize), Box<dyn Error>> {
        let client_tcp_stream = &mut self.client_connections.get_mut(token).unwrap().tcp_stream;
        let mut connection_closed = false;
        let mut bytes_read: usize = 0;
        loop {
            // write starting from where we left off
            match client_tcp_stream.read(&mut received_data[bytes_read..]) {
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
        Ok((connection_closed, bytes_read))
    }

    fn handle_client_message(
        &mut self,
        token: &Token,
        received_data: &mut [u8],
        bytes_read: usize,
    ) {
        let client_connection = self.client_connections.get_mut(token).unwrap();
        if let Ok(str_buf) = std::str::from_utf8(&received_data[..bytes_read]) {
            let message = Message {
                from: client_connection.address,
                content: String::from(str_buf.trim_end()),
            };

            self.queue_message_for_other_peers(message);
        } else {
            warn!(
                "Dropping message with none UTF-8 content from {}",
                client_connection.address
            );
        }
    }

    fn queue_message_for_other_peers(&mut self, message: Message) {
        info!("Queued new message: {}", message);
        let message = Rc::new(RefCell::new(message));
        for (token, connection) in &mut self.client_connections {
            if connection.tcp_stream.peer_addr().unwrap() != message.borrow().from {
                connection.pending_messages.push_front(message.clone());
            }

            self.poll.registry().reregister(
                &mut connection.tcp_stream,
                *token,
                Interest::READABLE.add(Interest::WRITABLE),
            ).expect(&*format!("Couldn't re-register {}", connection.address.to_string()));
        }
    }

    fn write_pending_messages_to_client(&mut self, token: &Token) -> Result<(), Box<dyn Error>> {
        let client_connection = self.client_connections.get_mut(token).unwrap();
        let messages = &mut client_connection.pending_messages;

        // would it be better to first just parse all the messages to
        // one [u8] buf just write the buf in one go? i guess it would
        while !messages.is_empty() {
            let message = match messages.pop_back() {
                Some(message) => message,
                None => continue,
            };

            let message = message.borrow();
            let message = format!("{}: {}\n", message.from, message.content);

            loop {
                info!("Writing: {}", message);
                match client_connection.tcp_stream.write(message.as_bytes()) {
                    // We want to write the entire `DATA` buffer in a single go. If we
                    // write less we'll return a short write error (same as
                    // `io::Write::write_all` does).
                    Ok(n) if n < message.len() => {
                        warn!("Couldn't write all bytes for message: {}", message);
                        continue;
                    }
                    Ok(_) => break,
                    // Would block "errors" are the OS's way of saying that the
                    // connection is not actually ready to perform this I/O operation.
                    Err(ref err) if Self::would_block(err) => {}
                    // Got interrupted (how rude!), we'll try again.
                    Err(ref err) if Self::interrupted(err) => continue,
                    // Other errors we'll consider fatal.
                    Err(err) => return Err(Box::new(err)),
                }
            }
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
