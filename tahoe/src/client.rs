use std::str::FromStr;
use std::io;
use std::io::Read;
use std::fs::{File, Metadata};
use std::collections::HashMap;
use std::time::UNIX_EPOCH;
use std::result;
use std::iter::FromIterator;
use std::hash::Hasher;

use tokio_core::reactor;

use hyper::client::{Client, HttpConnector, Request};
use hyper::{Body, Chunk, Method, Uri};

use futures::{Future, Sink, Stream};

use serde::{Serialize, Serializer};
use serde::ser::SerializeMap;
use serde_json;

use threadpool::ThreadPool;

use seahash::SeaHasher;

use errors::*;

#[derive(Serialize, Clone)]
pub struct DirNodeInner {
    ro_uri: String,
    metadata: HashMap<String, u64>,
}

#[derive(Serialize, Clone)]
pub enum NodeType {
    #[serde(rename = "dirnode")]
    Dir,
    #[serde(rename = "filenode")]
    File,
}

pub struct Dir {
    inner: Vec<(String, DirNode)>,
    hasher: SeaHasher,
}

impl Dir {
    fn new() -> Self {
        Dir {
            inner: Vec::new(),
            hasher: SeaHasher::new(),
        }
    }

    fn push(&mut self, value: (String, DirNode)) {
        self.hasher.write(value.0.as_bytes());
        self.hasher.write(value.1.uri().as_bytes());
        self.inner.push(value)
    }

    pub fn hash(&self) -> u64 {
        self.hasher.finish()
    }
}

impl Serialize for Dir {
    fn serialize<S>(&self, serializer: S) -> result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map = serializer.serialize_map(Some(self.inner.len()))?;
        for &(ref k, ref v) in &self.inner {
            map.serialize_entry(&k, &v)?;
        }
        map.end()
    }
}

impl FromIterator<(String, DirNode)> for Dir {
    fn from_iter<I: IntoIterator<Item = (String, DirNode)>>(iter: I) -> Self {
        let mut dir = Dir::new();
        for item in iter {
            dir.push(item);
        }

        dir
    }
}

#[derive(Serialize, Clone)]
pub struct DirNode(NodeType, DirNodeInner);

impl DirNode {
    pub fn new(ro_uri: String, meta: io::Result<Metadata>) -> Self {
        let nodetype = if ro_uri.starts_with("URI:DIR") {
            NodeType::Dir
        } else {
            NodeType::File
        };
        let mut metadata = HashMap::new();
        if let Ok(meta) = meta {
            if let Ok(created) = meta.created() {
                if let Ok(ctime) = created.duration_since(UNIX_EPOCH) {
                    metadata.insert(String::from("ctime"), ctime.as_secs());
                }
            }
            if let Ok(modified) = meta.modified() {
                if let Ok(mtime) = modified.duration_since(UNIX_EPOCH) {
                    metadata.insert(String::from("mtime"), mtime.as_secs());
                }
            }
        }
        DirNode(nodetype, DirNodeInner { ro_uri, metadata })
    }

    fn uri(&self) -> &str {
        &self.1.ro_uri
    }
}

#[derive(Clone)]
pub struct Tahoe {
    client: Client<HttpConnector>,
    pool: ThreadPool,
    base: String,
    file_uri: Uri,
    dir_uri: Uri,
}

impl Tahoe {
    pub fn new(num_threads: usize, handle: &reactor::Handle, base: Option<&str>) -> Result<Self> {
        let pool = ThreadPool::new(num_threads);
        let base = base.unwrap_or("127.0.0.1:3456");
        let base_str = &format!("http://{}/uri", base);
        let file_uri = Uri::from_str(base_str).chain_err(|| "failed to parse base")?;
        let dir_uri = Uri::from_str(&format!("{}?t=mkdir-immutable", base_str))
            .chain_err(|| "failed to add mkdir")?;
        let client = Client::new(handle);

        info!("Connecting to {} with {} threads", base_str, num_threads);
        Ok(Tahoe {
            client,
            pool,
            base: base_str.clone(),
            file_uri,
            dir_uri,
        })
    }

    pub fn threads(&self) -> usize {
        self.pool.max_count()
    }

    pub fn attach(
        &self,
        dircap: &str,
        path: &str,
        filecap: &str,
    ) -> Result<impl Future<Item = (), Error = Error>> {
        let body: Body = String::from(filecap).into();
        let uri = Uri::from_str(&format!("{}/{}/{}?t=uri", self.base, dircap, path))
            .chain_err(|| "failed to form url")?;

        let mut request = Request::new(Method::Put, uri);
        request.set_body(body);

        Ok(self.client
            .request(request)
            .map_err(upload_err)
            .and_then(|res| {
                if res.status().is_success() {
                    Ok(())
                } else {
                    bail!(ErrorKind::Tahoe(res.status()))
                }
            }))
    }

    pub fn upload_dir(&self, dir: &Dir) -> Result<impl Future<Item = String, Error = Error>> {
        let body: Body = serde_json::to_vec(dir)
            .chain_err(|| "Failed to serialize directory")?
            .into();

        let mut request = Request::new(Method::Post, self.dir_uri.clone());
        request.set_body(body);

        Ok(self.client
            .request(request)
            .map_err(upload_err)
            .and_then(|res| {
                if res.status().is_success() {
                    Ok(res)
                } else {
                    bail!(ErrorKind::Tahoe(res.status()))
                }
            })
            .and_then(|res| res.body().concat2().map_err(upload_err))
            .and_then(|b| String::from_utf8(b.to_vec()).map_err(upload_err))) // TODO: Don't clone here
    }

    pub fn upload_file(&self, mut file: File) -> impl Future<Item = String, Error = Error> {
        let (tx, body) = Body::pair();
        let mut request = Request::new(Method::Put, self.file_uri.clone());
        request.set_body(body);

        self.pool.execute(move || {
            let mut tx_body = tx;
            let mut buf = [0u8; 1024 * 1024];

            loop {
                match file.read(&mut buf) {
                    Err(_) => {
                        break;
                    }
                    Ok(0) => {
                        tx_body.close().expect("panic closing");
                        break;
                    }
                    Ok(n) => {
                        let chunk: Chunk = buf[0..n].to_vec().into();
                        match tx_body.send(Ok(chunk)).wait() {
                            Ok(t) => {
                                tx_body = t;
                            }
                            Err(_) => {
                                break;
                            }
                        };
                    }
                }
            }
        });

        self.client
            .request(request)
            .map_err(upload_err)
            .and_then(|res| {
                if res.status().is_success() {
                    Ok(res)
                } else {
                    bail!(ErrorKind::Tahoe(res.status()))
                }
            })
            .and_then(|res| {
                res.body()
                    .concat2()
                    .map_err(|e| Error::with_chain(e, "Failed to read response"))
            })
            .and_then(|b| {
                String::from_utf8(b.to_vec())
                    .map_err(|e| Error::with_chain(e, "Failed to parse response into string"))
            }) // TODO: Don't clone here
    }
}

fn upload_err<E>(error: E) -> Error
where
    E: ::std::error::Error + Send + 'static,
{
    Error::with_chain(error, "failed to upload file")
}
