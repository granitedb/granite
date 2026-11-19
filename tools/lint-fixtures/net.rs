//! Rejected: using the network outside the io crate.

use std::net::{SocketAddr, TcpStream};

pub fn connect(address: SocketAddr) -> bool {
    TcpStream::connect(address).is_ok()
}
