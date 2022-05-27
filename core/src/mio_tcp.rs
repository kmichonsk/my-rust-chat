use mio::event::Event;
use mio::net::{TcpListener, TcpStream};
use mio::{Events, Interest, Poll, Registry, Token};
use std::collections::HashMap;
use std::error::Error;
use std::io::{Read, Write};
use std::time::Duration;

pub fn run_chat() -> Result<(), Box<dyn Error>> {
    // Some tokens to allow us to identify which event is for which socket.
    // Token is just a tuple struct for usize
    // similar concept to file descriptors but made for use
    // with connections I guess
    const SERVER: Token = Token(0);
    const TIMEOUT: Option<Duration> = Some(Duration::from_secs(32));

    // Create a poll instance.
    let mut poll = Poll::new()?;
    // Create storage for events.
    let mut events = Events::with_capacity(128);

    let addr = "127.0.0.1:9000".parse()?;
    let mut server = TcpListener::bind(addr)?;

    // register our event source for polling
    poll.registry()
        .register(&mut server, SERVER, Interest::READABLE)?;

    let mut connections = HashMap::new();
    // This could be wrapped in something a bit nicer
    let mut unique_token = Token(SERVER.0 + 1);

    println!("You can connect to the server using `nc`:");
    println!(" $ nc 127.0.0.1 9000");
    println!("You'll see our welcome message and anything you type will be printed here.");

    // our event loop:
    loop {
        // Poll all registered event sources for events
        poll.poll(&mut events, TIMEOUT)?;

        // Process each event.
        for event in events.iter() {
            eprintln!("New event!\n\t{:?}", event);

            // We can use the token we previously provided to `register` to
            // determine for which socket the event is.
            match event.token() {
                SERVER => handle_server_token_event(
                    &mut poll,
                    &mut server,
                    &mut connections,
                    &mut unique_token,
                )?,
                _ => handle_existing_connection_event(
                    &mut poll,
                    &mut server,
                    &mut connections,
                    &mut unique_token,
                    event,
                )?,
            }
        }
    }
}

fn handle_server_token_event(
    poll: &mut Poll,
    server: &mut TcpListener,
    connections: &mut HashMap<Token, TcpStream>,
    mut unique_token: &mut Token,
) -> Result<(), Box<dyn Error>> {
    // i hate that it's done using a loop
    loop {
        // Received an event for the TCP server socket, which
        // indicates we can accept a connection.
        let (mut connection, address) = match server.accept() {
            Ok((connection, address)) => (connection, address),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                // If we get a `WouldBlock` error we know our
                // listener has no more incoming connections queued,
                // so we can return to polling and wait for some
                // more.
                break;
            }
            Err(e) => {
                // If it was any other kind of error, something went
                // wrong and we terminate with an error.
                return Err(Box::new(e));
            }
        };

        println!("Accepted connection from: {}", address);
        // Create a new token for this connection
        let next = unique_token.0;
        unique_token.0 += 1;
        let token = Token(next);

        // Register this connection so we can poll events from it later
        poll.registry().register(
            &mut connection,
            token,
            // we also want to later write to it
            Interest::READABLE.add(Interest::WRITABLE),
        )?;

        // we will later want to do something with this connection so let's
        // save it in some runtime memory
        connections.insert(token, connection);
    }
    Ok(())
}

fn handle_existing_connection_event(
    poll: &mut Poll,
    server: &mut TcpListener,
    connections: &mut HashMap<Token, TcpStream>,
    mut unique_token: &mut Token,
    event: &Event,
) -> Result<(), Box<dyn Error>> {
    // We got an event on one of the connections
    let done = if let Some(connection) = connections.get_mut(&event.token()) {
        handle_connection_event(poll.registry(), connection, &event)?
    } else {
        eprintln!("Weird event without associated connection: ${:?}", event);
        false
    };

    // connection is "done"
    if done {
        if let Some(mut connection) = connections.remove(&event.token()) {
            poll.registry().deregister(&mut connection)?;
        }
    }
    Ok(())
}

/// Returns `true` if the connection is done.
fn handle_connection_event(
    registry: &Registry,
    connection: &mut TcpStream,
    event: &Event,
) -> std::io::Result<bool> {
    const DATA: &[u8] = b"Hello world!\n";
    if event.is_writable() {
        // We can (maybe) write to the connection.
        match connection.write(DATA) {
            // We want to write the entire `DATA` buffer in a single go. If we
            // write less we'll return a short write error (same as
            // `io::Write::write_all` does).
            Ok(n) if n < DATA.len() => return Err(std::io::ErrorKind::WriteZero.into()),
            Ok(_) => {
                // After we've written something we'll reregister the connection
                // to only respond to readable events.
                registry.reregister(connection, event.token(), Interest::READABLE)?
            }
            // Would block "errors" are the OS's way of saying that the
            // connection is not actually ready to perform this I/O operation.
            Err(ref err) if would_block(err) => {}
            // Got interrupted (how rude!), we'll try again.
            Err(ref err) if interrupted(err) => {
                return handle_connection_event(registry, connection, event)
            }
            // Other errors we'll consider fatal.
            Err(err) => return Err(err),
        }
    }

    if event.is_readable() {
        let mut connection_closed = false;
        let mut received_data = vec![0; 4096];
        let mut bytes_read = 0;
        // We can (maybe) read from the connection.
        loop {
            match connection.read(&mut received_data[bytes_read..]) {
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
                Err(ref err) if would_block(err) => break,
                Err(ref err) if interrupted(err) => continue,
                // Other errors we'll consider fatal.
                Err(err) => return Err(err),
            }
        }

        if bytes_read != 0 {
            let received_data = &received_data[..bytes_read];
            if let Ok(str_buf) = std::str::from_utf8(received_data) {
                println!("Received data: {}", str_buf.trim_end());
            } else {
                println!("Received (none UTF-8) data: {:?}", received_data);
            }
        }

        if connection_closed {
            println!("Connection closed");
            return Ok(true);
        }
    }

    Ok(false)
}

fn would_block(err: &std::io::Error) -> bool {
    err.kind() == std::io::ErrorKind::WouldBlock
}

fn interrupted(err: &std::io::Error) -> bool {
    err.kind() == std::io::ErrorKind::Interrupted
}
