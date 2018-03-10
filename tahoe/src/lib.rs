#![feature(conservative_impl_trait)]
extern crate futures;
extern crate hyper;
extern crate serde;
extern crate serde_json;
extern crate threadpool;
extern crate tokio_core;
extern crate url;

#[macro_use]
extern crate serde_derive;

#[macro_use]
extern crate error_chain;

#[macro_use]
extern crate log;

pub mod errors {
    use hyper;

    error_chain!{
        errors {
            Tahoe(s: hyper::StatusCode) {
                description("Tahoe error"),
                display("Tahoe returned {} {}", s.as_u16(), s.canonical_reason().unwrap_or("(unknown)"))
            }
        }
    }
}

pub mod client;
