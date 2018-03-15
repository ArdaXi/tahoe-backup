table! {
    caps (fileid) {
        fileid -> Integer,
        filecap -> Text,
    }
}

table! {
    directories (dirhash) {
        dirhash -> BigInt,
        dircap -> Text,
        last_uploaded -> Nullable<Timestamp>,
    }
}

table! {
    last_upload (fileid) {
        fileid -> Integer,
        last_uploaded -> Nullable<Timestamp>,
    }
}

table! {
    local_files (path) {
        path -> Text,
        size -> BigInt,
        mtime -> BigInt,
        ctime -> BigInt,
        fileid -> Integer,
    }
}

table! {
    version (dbversion) {
        #[sql_name = "version"]
        dbversion -> Integer,
    }
}

joinable!(last_upload -> caps (fileid));
joinable!(local_files -> caps (fileid));

allow_tables_to_appear_in_same_query!(caps, directories, last_upload, local_files, version,);
