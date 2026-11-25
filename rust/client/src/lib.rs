//! The client side of cwbcode's API: a connection to the backend over HTTP/3, or over HTTP/2
//! where QUIC is not available, trusting exactly the backend's self-signed certificate.

pub mod net;
