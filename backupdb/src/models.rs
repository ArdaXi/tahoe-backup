use std::time::SystemTime;

use schema::*;

#[derive(Queryable)]
#[primary_key(fileid)]
pub struct Cap {
    pub fileid: i32,
    pub filecap: String,
}

#[derive(Insertable)]
#[table_name = "directories"]
#[primary_key(dirhash)]
pub struct Directory {
    pub dirhash: i64,
    pub dircap: String,
    pub last_uploaded: SystemTime,
}

#[derive(Queryable, Insertable)]
#[table_name = "last_upload"]
#[primary_key(fileid)]
pub struct LastUpload {
    pub fileid: i32,
    pub last_uploaded: SystemTime,
}

#[derive(Queryable, Identifiable, Insertable)]
#[table_name = "local_files"]
#[primary_key(path)]
pub struct LocalFile {
    pub path: String,
    pub size: i64,
    pub mtime: i64,
    pub ctime: i64,
    pub fileid: i32,
}

#[derive(Queryable)]
pub struct Version {
    #[table_name = "version"]
    #[primary_key(version)]
    pub version: i32,
}
