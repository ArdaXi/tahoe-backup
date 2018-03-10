use diesel;
use diesel::{insert_into, select, sql_types};
use diesel::prelude::*;
use diesel::sqlite::SqliteConnection;
use diesel::result::Error::DatabaseError;
use diesel::result::DatabaseErrorKind::UniqueViolation;
use models::*;
use errors::*;

embed_migrations!();

pub struct BackupDB {
    connection: SqliteConnection,
}

impl BackupDB {
    pub fn new(database_url: &str) -> Result<BackupDB> {
        SqliteConnection::establish(database_url)
            .chain_err(|| ErrorKind::Connection(String::from(database_url)))
            .and_then(|connection| {
                embedded_migrations::run(&connection)
                    .chain_err(|| "Failed to run migrations.")
                    .map(|_| connection)
            })
            .map(|connection| BackupDB { connection })
    }

    pub fn check_file(&self, path: &str, size: i64, ctime: i64, mtime: i64) -> Option<String> {
        use schema::local_files::all_columns;
        use schema::local_files::dsl::local_files;
        use schema::caps::dsl::{caps, filecap};

        local_files
            .find(path)
            .inner_join(caps)
            .select((all_columns, filecap))
            .first::<(LocalFile, String)>(&self.connection)
            .ok()
            .and_then(|(file, cap)| {
                if file.size != size || file.ctime != ctime || file.mtime != mtime {
                    diesel::delete(&file).execute(&self.connection);
                    return None;
                }
                Some(cap)
            })
    }

    pub fn add_file(
        &self,
        cap: &str,
        path: String,
        size: i64,
        ctime: i64,
        mtime: i64,
    ) -> Result<()> {
        use schema::caps::dsl::fileid as capid;
        use schema::caps::dsl::{caps, filecap};
        use schema::last_upload::dsl::{fileid, last_upload};
        use schema::local_files::dsl::local_files;
        no_arg_sql_function!(last_insert_rowid, sql_types::Integer, "last_insert_rowid");

        let id = match insert_into(caps)
            .values(filecap.eq(cap))
            .execute(&self.connection)
        {
            Ok(_) => select(last_insert_rowid).first(&self.connection)?,
            Err(DatabaseError(UniqueViolation, _)) => caps.filter(filecap.eq(cap))
                .select(capid)
                .first(&self.connection)?,
            Err(e) => return Err(Error::with_chain(e, "Failed to insert cap")),
        };
        diesel::delete(last_upload.find(fileid)).execute(&self.connection);
        insert_into(last_upload)
            .values(fileid.eq(id))
            .execute(&self.connection)
            .chain_err(|| "Failed to insert last upload")?;
        diesel::delete(local_files.find(&path)).execute(&self.connection);
        insert_into(local_files)
            .values(&LocalFile {
                fileid: id,
                path,
                size,
                ctime,
                mtime,
            })
            .execute(&self.connection)
            .chain_err(|| "Failed to insert local file")?;
        Ok(())
    }
}
