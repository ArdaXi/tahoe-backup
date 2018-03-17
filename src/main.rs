extern crate backupdb;
extern crate chrono;
extern crate env_logger;
extern crate filetime;
extern crate futures;
extern crate globset;
extern crate indicatif;
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
            GlobParse(glob: String) {
                description("Couldn't parse glob"),
                display("Couldn't parse glob: '{}'", glob),
            }
            UnknownFile(path: String) {
                description("Unexpected file"),
                display("Unexpected file: '{}'", path),
            }
        }
    }
}

use std::{env, fs, io};
use std::fs::File;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;

use tokio_core::reactor::Core;

use futures::{future, stream, Future, IntoFuture, Stream};

use tahoe::client::{Dir, DirNode, Tahoe};

use backupdb::BackupDB;

use errors::*;

use filetime::FileTime;

use clap::Arg;

use chrono::Utc;

use globset::{Glob, GlobSet, GlobSetBuilder};

use indicatif::{MultiProgress, ProgressBar, ProgressStyle};

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

fn style() -> ProgressStyle {
    ProgressStyle::default_bar()
        .template(
            "{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta}) {wide_msg}",
        )
        .progress_chars("#>-")
}

fn dir_style() -> ProgressStyle {
    ProgressStyle::default_spinner().template("{spinner:.green} [{elapsed_precise}] {wide_msg}")
}

fn finished_style() -> ProgressStyle {
    ProgressStyle::default_bar()
        .template(
            "{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {total_bytes} {wide_msg}",
        )
        .progress_chars("#>-")
}

fn upload<'a>(
    progress: &'a MultiProgress,
    globset: &'a Option<GlobSet>,
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
        let size = metadata.len();
        let ctime = FileTime::from_creation_time(&metadata)
            .unwrap_or(FileTime::zero())
            .seconds() as i64;
        let mtime = FileTime::from_last_modification_time(&metadata).seconds() as i64;

        if let Some(cap) = db.check_file(&path, size as i64, ctime, mtime) {
            info!("Skipping '{}'", path);
            return Box::new(future::ok(Ok(cap)));
        }

        let f = match File::open(&path).chain_err(|| ErrorKind::FileOpen(path.clone())) {
            Ok(x) => x,
            Err(e) => return Box::new(future::ok(Err(e))),
        };
        info!("Uploading file '{}'", &path);
        let logpath = path.clone();
        let pb = Arc::new(progress.add(ProgressBar::new(size)));
        pb.set_style(style());
        pb.set_message(&path);
        let pb2 = pb.clone();
        return Box::new(
            client
                .upload_file(f, move |n| pb2.inc(n as u64))
                .inspect(move |cap| {
                    pb.set_style(finished_style());
                    pb.finish_and_clear();
                    info!("'{}' -> '{}'", &logpath, cap);
                    ok_or_log(db.add_file(&cap, logpath, size as i64, ctime, mtime));
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
        let pb = progress.add(ProgressBar::new_spinner());
        pb.set_style(dir_style());
        pb.set_message(&path);
        pb.enable_steady_tick(100);
        return Box::new(
            stream::iter_ok(
                files
                    .filter_map(ok_or_log)
                    .filter(move |entry| {
                        if let &Some(ref globs) = globset {
                            !globs.is_match(entry.path())
                        } else {
                            true
                        }
                    })
                    .map(move |entry| {
                        let path = entry.path().to_string_lossy().into_owned();
                        upload(
                            progress,
                            globset,
                            client,
                            db,
                            path.clone(),
                            entry.metadata(),
                        ).map(move |f| {
                            f.map(|res| {
                                (
                                    entry
                                        .path()
                                        .file_name()
                                        .unwrap()
                                        .to_string_lossy()
                                        .into_owned(),
                                    DirNode::new(res, entry.metadata()),
                                )
                            })
                        })
                    }),
            ).buffered(client.threads())
                .filter_map(ok_or_log)
                .collect()
                .inspect(move |_| info!("Uploading dir '{}'", path))
                .map(|v| v.iter().cloned().collect())
                .and_then(move |dir| upload_dir(pb, client, db, dir, logpath)),
        );
    }

    Box::new(future::ok(Err(ErrorKind::UnknownFile(path).into())))
}

fn upload_dir<'a>(
    pb: ProgressBar,
    client: &'a Tahoe,
    db: &'a BackupDB,
    dir: Dir,
    path: String,
) -> Box<Future<Item = Result<String>, Error = Error> + 'a> {
    let hash = dir.hash() as i64;
    match db.check_dir(hash) {
        Some(cap) => {
            info!("Reusing directory '{}'", path);
            pb.finish_and_clear();
            Box::new(future::ok(Ok(cap)))
        }
        None => Box::new(
            client
                .upload_dir(&dir)
                .into_future()
                .flatten()
                .inspect(move |cap| {
                    ok_or_log(db.add_dir(hash, &cap));
                    pb.finish_and_clear();
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

fn build_globset<'a, I: Iterator<Item = &'a str>>(iter: I) -> Result<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for item in iter {
        builder.add(Glob::new(item).chain_err(|| ErrorKind::GlobParse(String::from(item)))?);
    }
    builder.build().chain_err(|| "Failed to build globset")
}

fn run() -> Result<()> {
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
            Arg::with_name("exclude")
                .short("x")
                .long("exclude")
                .help("Ignore files matching a glob pattern")
                .multiple(true)
                .takes_value(true),
        )
        .arg(
            Arg::with_name("path")
                .help("The folder to backup")
                .required(true),
        )
        .arg(
            Arg::with_name("target")
                .help("The capability to upload into")
                .required(true),
        )
        .get_matches();
    let threads: usize = matches.value_of("threads").unwrap().parse().unwrap_or(4);
    let path =
        fs::canonicalize(matches.value_of_os("path").unwrap()).chain_err(|| "Couldn't find path")?;
    let database = matches.value_of("database").unwrap();
    let target = matches.value_of("target").unwrap();
    let mut core = Core::new().unwrap();
    let client = Tahoe::new(threads, &core.handle(), None).unwrap();
    let db = BackupDB::new(database).unwrap();
    let globset = match matches.values_of("exclude") {
        Some(globs) => Some(build_globset(globs)?),
        None => None,
    };
    let mp = Arc::new(MultiProgress::new());
    let work = upload(
        &mp,
        &globset,
        &client,
        &db,
        path.to_string_lossy().into_owned(),
        fs::symlink_metadata(path),
    ).and_then(|res| {
        res.map(|cap| {
            let datetime = format!("Archives/{}", Utc::now().to_rfc3339());
            info!("Adding link 'Latest' and '{}'", datetime);
            client
                .attach(target, &datetime, &cap)
                .unwrap()
                .map_err(|e| Error::with_chain(e, "failed to attach archive"))
                .inspect(|_| info!("Added Archives link"))
                .join(
                    client
                        .attach(target, "Latest", &cap)
                        .unwrap()
                        .map_err(|e| Error::with_chain(e, "failed to attach archive"))
                        .inspect(|_| info!("Added Latest link")),
                )
        })
    })
        .flatten();
    let bar = mp.add(ProgressBar::hidden());
    let mp2 = mp.clone();
    thread::spawn(move || mp2.join());
    core.run(work)?;
    bar.finish();
    Ok(())
}

fn main() {
    run().map_err(log_err).ok();
}
