#![allow(unused_attributes)]
#![feature(custom_attribute)]
#[macro_use]
extern crate diesel;

#[macro_use]
extern crate diesel_migrations;

extern crate dotenv;

#[macro_use]
extern crate error_chain;

pub mod schema;
pub mod models;
pub mod backupdb;

pub use backupdb::*;

pub mod errors {
    error_chain!{
        foreign_links {
            Diesel(::diesel::result::Error);
        }
        errors {
            Connection(url: String) {
                description("Failed to connect to database"),
                display("Failed to connect to database: '{}'", url),
            }
        }
    }
}
