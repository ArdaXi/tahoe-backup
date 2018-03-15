CREATE TABLE version
(
 version integer PRIMARY KEY NOT NULL -- contains one row, set to 1
);

CREATE TABLE local_files
(
  path  varchar(1024) PRIMARY KEY NOT NULL, -- index, this is an absolute UTF-8-encoded local filename
  size  integer NOT NULL,         -- os.stat(fn)[stat.ST_SIZE]
  mtime integer NOT NULL,          -- os.stat(fn)[stat.ST_MTIME]
  ctime integer NOT NULL,          -- os.stat(fn)[stat.ST_CTIME]
  fileid integer NOT NULL,
  FOREIGN KEY(fileid) REFERENCES caps(fileid)
);

CREATE TABLE caps
(
 fileid integer PRIMARY KEY NOT NULL,
 filecap varchar(256) UNIQUE NOT NULL    -- URI:CHK:...
);

CREATE TABLE last_upload
(
 fileid INTEGER PRIMARY KEY NOT NULL,
 last_uploaded TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
 FOREIGN KEY(fileid) REFERENCES caps(fileid)
);

CREATE TABLE directories
(
 dirhash integer PRIMARY KEY NOT NULL,
 dircap varchar(256) NOT NULL,
 last_uploaded TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);
