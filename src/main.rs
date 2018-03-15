extern crate backupdb;
extern crate env_logger;
extern crate filetime;
extern crate futures;
extern crate tahoe;
extern crate tokio_core;

#[macro_use]
extern crate log;

#[macro_use]
extern crate error_chain;

#[macro_use]
extern crate clap;

mod errors {
    use tahoe;
    use backupdb;
    error_chain!{
        links {
            Tahoe(tahoe::errors::Error, tahoe::errors::ErrorKind);
            BackupDB(backupdb::errors::Error, backupdb::errors::ErrorKind);
        }
        foreign_links {
            Io(::std::io::Error);
        }
        errors {
            FileOpen(path: String) {
                description("Couldn't open file"),
                display("Couldn't open file: '{}'", path),
            }
            ReadMetadata(path: String) {
                description("Couldn't read metadata"),
                display("Couldn't read metadata: '{}'", path),
            }
            FileUpload(path: String) {
                description("Couldn't upload file"),
                display("Couldn't upload file: '{}'", path),
            }
        }
    }
}

use std::{env, fs, io};
use std::fs::File;
use std::path::PathBuf;

use tokio_core::reactor::Core;

use futures::{future, stream, Future, IntoFuture, Stream};

use tahoe::client::{Dir, DirNode, Tahoe};

use backupdb::BackupDB;

use errors::*;

use filetime::FileTime;

use clap::Arg;

fn ok_or_log<T, E>(res: std::result::Result<T, E>) -> Option<T>
where
    E: Into<Error>,
{
    match res {
        Ok(x) => Some(x),
        Err(e) => {
            log_err(e);
            None
        }
    }
}

fn upload<'a>(
    client: &'a Tahoe,
    db: &'a BackupDB,
    path: String,
    metadata: io::Result<fs::Metadata>,
) -> Box<Future<Item = Result<String>, Error = Error> + 'a> {
    if metadata.is_err() {
        return Box::new(future::ok(
            metadata
                .map(|_| String::new())
                .chain_err(|| ErrorKind::ReadMetadata(path.clone())),
        ));
    }

    let metadata = metadata.unwrap();

    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        return Box::new(future::ok(Err("not following symlink".into())));
    }

    if file_type.is_file() {
        let size = metadata.len() as i64;
        let ctime = FileTime::from_creation_time(&metadata)
            .unwrap_or(FileTime::zero())
            .seconds() as i64;
        let mtime = FileTime::from_last_modification_time(&metadata).seconds() as i64;

        if let Some(cap) = db.check_file(&path, size, ctime, mtime) {
            info!("Skipping '{}'", path);
            return Box::new(future::ok(Ok(cap)));
        }

        let f = match File::open(&path).chain_err(|| ErrorKind::FileOpen(path.clone())) {
            Ok(x) => x,
            Err(e) => return Box::new(future::ok(Err(e))),
        };
        info!("Uploading file '{}'", &path);
        let logpath = path.clone();
        return Box::new(
            client
                .upload_file(f)
                .inspect(move |cap| {
                    info!("'{}' -> '{}'", &logpath, cap);
                    ok_or_log(db.add_file(&cap, logpath, size, ctime, mtime));
                    ()
                })
                .map_err(move |e| Error::with_chain(e, ErrorKind::FileUpload(path)))
                .map(Ok),
        );
    }

    if file_type.is_dir() {
        let files = fs::read_dir(path.clone());
        if files.is_err() {
            return Box::new(future::ok(
                files
                    .map(|_| String::new())
                    .chain_err(|| "couldn't read dir"),
            ));
        }

        let files = files.unwrap();
        let logpath = path.clone();
        return Box::new(
            stream::iter_ok(files.filter_map(ok_or_log).map(move |entry| {
                let path = entry.path().to_string_lossy().into_owned();
                upload(client, db, path.clone(), entry.metadata())
                    .map(move |f| f.map(|res| (path, DirNode::new(res, entry.metadata()))))
            })).buffered(client.threads())
                .filter_map(ok_or_log)
                .collect()
                .inspect(move |_| info!("Uploading dir '{}'", path))
                .map(|v| v.iter().cloned().collect())
                .and_then(move |dir| upload_dir(client, db, dir, logpath)),
        );
    }

    Box::new(future::err("Unexpected file".into()))
}

fn upload_dir<'a>(
    client: &'a Tahoe,
    db: &'a BackupDB,
    dir: Dir,
    path: String,
) -> Box<Future<Item = Result<String>, Error = Error> + 'a> {
    let hash = dir.hash() as i64;
    match db.check_dir(hash) {
        Some(cap) => {
            info!("Reusing directory '{}'", path);
            Box::new(future::ok(Ok(cap)))
        }
        None => Box::new(
            client
                .upload_dir(&dir)
                .into_future()
                .flatten()
                .inspect(move |cap| {
                    ok_or_log(db.add_dir(hash, &cap));
                    info!("'{}' -> '{}'", path, cap)
                })
                .map(Ok)
                .map_err(|e| Error::with_chain(e, "couldn't upload dir")),
        ),
    }
}

fn log_err<E>(err: E)
where
    E: Into<Error>,
{
    for (i, e) in err.into().iter().enumerate() {
        warn!("{}{}", " ".repeat(i), e)
    }
}

fn main() {
    env_logger::init();
    let mut default_database = env::home_dir().unwrap_or_else(PathBuf::new);
    default_database.push(".tahoe/private/rust-backupdb.sqlite");
    let default_database = default_database.into_os_string();
    let matches = app_from_crate!()
        .arg(
            Arg::with_name("threads")
                .short("t")
                .long("threads")
                .help("Sets the amount of threads to use")
                .default_value("4"),
        )
        .arg(
            Arg::with_name("database")
                .short("d")
                .long("database")
                .help("Location of the database file")
                .default_value_os(&default_database)
                .takes_value(true),
        )
        .arg(
            Arg::with_name("path")
                .help("The folder to backup")
                .required(true),
        )
        .get_matches();
    let threads: usize = matches.value_of("threads").unwrap().parse().unwrap_or(4);
    let path = fs::canonicalize(matches.value_of_os("path").unwrap()).unwrap();
    let database = matches.value_of("database").unwrap();
    let mut core = Core::new().unwrap();
    let client = Tahoe::new(threads, &core.handle(), None).unwrap();
    let db = BackupDB::new(database).unwrap();
    let cap = upload(
        &client,
        &db,
        path.to_string_lossy().into_owned(),
        fs::symlink_metadata(path),
    );
    let res = match core.run(cap) {
        Ok(Ok(x)) => x,
        Ok(Err(e)) | Err(e) => return log_err(e),
    };
    println!("{}", res);
}
