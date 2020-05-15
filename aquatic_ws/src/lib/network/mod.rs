use std::time::Duration;
use std::io::ErrorKind;

use hashbrown::HashMap;
use native_tls::TlsAcceptor;

use mio::{Events, Poll, Interest, Token};
use mio::net::TcpListener;

use crate::common::*;
use crate::config::Config;
use crate::protocol::*;

pub mod connection;
pub mod utils;

use connection::*;
use utils::*;


pub fn run_socket_worker(
    config: Config,
    socket_worker_index: usize,
    in_message_sender: InMessageSender,
    out_message_receiver: OutMessageReceiver,
){
    let poll_timeout = Duration::from_millis(
        config.network.poll_timeout_milliseconds
    );

    let mut listener = TcpListener::from_std(create_listener(&config));
    let mut poll = Poll::new().expect("create poll");
    let mut events = Events::with_capacity(config.network.poll_event_capacity);

    poll.registry()
        .register(&mut listener, Token(0), Interest::READABLE)
        .unwrap();

    let opt_tls_acceptor = if config.network.use_tls {
        Some(create_tls_acceptor(&config))
    } else {
        None
    };

    let mut connections: ConnectionMap = HashMap::new();

    let mut poll_token_counter = Token(0usize);
    let mut iter_counter = 0usize;

    loop {
        poll.poll(&mut events, Some(poll_timeout))
            .expect("failed polling");
        
        let valid_until = ValidUntil::new(config.cleaning.max_connection_age);

        for event in events.iter(){
            let token = event.token();

            if token.0 == 0 {
                accept_new_streams(
                    &mut listener,
                    &mut poll,
                    &mut connections,
                    valid_until,
                    &mut poll_token_counter,
                );
            } else if event.is_readable(){
                run_handshakes_and_read_messages(
                    socket_worker_index,
                    &in_message_sender,
                    &opt_tls_acceptor,
                    &mut connections,
                    token,
                    valid_until,
                );
            }
        }

        send_out_messages(
            out_message_receiver.drain(),
            &mut connections
        );

        // Remove inactive connections, but not every iteration
        if iter_counter % 128 == 0 {
            remove_inactive_connections(&mut connections);
        }

        iter_counter = iter_counter.wrapping_add(1);
    }
}


fn accept_new_streams(
    listener: &mut TcpListener,
    poll: &mut Poll,
    connections: &mut ConnectionMap,
    valid_until: ValidUntil,
    poll_token_counter: &mut Token,
){
    loop {
        match listener.accept(){
            Ok((mut stream, _)) => {
                poll_token_counter.0 = poll_token_counter.0.wrapping_add(1);

                if poll_token_counter.0 == 0 {
                    poll_token_counter.0 = 1;
                }

                let token = *poll_token_counter;

                remove_connection_if_exists(connections, token);

                poll.registry()
                    .register(&mut stream, token, Interest::READABLE)
                    .unwrap();

                let connection = Connection::new(valid_until, stream);

                connections.insert(token, connection);
            },
            Err(err) => {
                if err.kind() == ErrorKind::WouldBlock {
                    break
                }

                eprint!("{}", err);
            }
        }
    }
}


/// On the stream given by poll_token, get TLS (if requested) and tungstenite
/// up and running, then read messages and pass on through channel.
pub fn run_handshakes_and_read_messages(
    socket_worker_index: usize,
    in_message_sender: &InMessageSender,
    opt_tls_acceptor: &Option<TlsAcceptor>, // If set, run TLS
    connections: &mut ConnectionMap,
    poll_token: Token,
    valid_until: ValidUntil,
){
    loop {
        if let Some(established_ws) = connections.get_mut(&poll_token)
            .and_then(Connection::get_established_ws)
        {
            use ::tungstenite::Error::Io;

            match established_ws.ws.read_message(){
                Ok(ws_message) => {
                    dbg!(ws_message.clone());
    
                    if let Some(in_message) = InMessage::from_ws_message(ws_message){
                        dbg!(in_message.clone());
    
                        let meta = ConnectionMeta {
                            worker_index: socket_worker_index,
                            poll_token: poll_token,
                            peer_addr: established_ws.peer_addr
                        };
    
                        in_message_sender.send((meta, in_message));
                    }
                },
                Err(Io(err)) if err.kind() == ErrorKind::WouldBlock => {
                    break
                }
                Err(err) => {
                    dbg!(err);
    
                    remove_connection_if_exists(connections, poll_token);
    
                    break;
                }
            }
        } else if let Some(connection) = connections.remove(&poll_token){
            let (opt_new_connection, stop_loop) = connection.advance_handshakes(
                opt_tls_acceptor,
                valid_until
            );

            if let Some(connection) = opt_new_connection {
                connections.insert(poll_token, connection);
            }

            if stop_loop {
                break;
            }
        }
    }
}


/// Read messages from channel, send to peers
pub fn send_out_messages(
    out_message_receiver: ::flume::Drain<(ConnectionMeta, OutMessage)>,
    connections: &mut ConnectionMap,
){
    for (meta, out_message) in out_message_receiver {
        let opt_established_ws = connections.get_mut(&meta.poll_token)
            .and_then(Connection::get_established_ws);
        
        if let Some(established_ws) = opt_established_ws {
            if established_ws.peer_addr != meta.peer_addr {
                eprintln!("socket worker: peer socket addrs didn't match");

                continue;
            }

            dbg!(out_message.clone());
        
            use ::tungstenite::Error::Io;

            match established_ws.ws.write_message(out_message.to_ws_message()){
                Ok(()) => {},
                Err(Io(err)) if err.kind() == ErrorKind::WouldBlock => {
                    continue;
                },
                Err(err) => {
                    dbg!(err);

                    remove_connection_if_exists(
                        connections,
                        meta.poll_token
                    );
                },
            }
        }
    }
}