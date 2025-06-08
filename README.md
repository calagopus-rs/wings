# wings-rs (experimental)

a rewrite of [pterodactyl wings](https://github.com/pterodactyl/wings) in the rust programming language. this rewrite aims to be 100% API compatible while implementing new features and better performance.

## added config options

```yml
api:
  # how many entries can be listed on the /list-directory API call
  directory_entry_limit: 10000
  # send server logs of an offline server when connecting to ws
  send_offline_server_logs: false
  # how many threads to use when searching files using file search
  file_search_threads: 4

system:
  # apply a real quota limit to each server
  # none, btrfs_subvolume, zfs_dataset, xfs_quota
  disk_limiter_mode: none

  sftp:
    # the algorithm to use for the ssh host key
    key_algorithm: ssh-ed25519
    # whether to disable password auth for the sftp server
    disable_password_auth: false
    # how many entries can be listed on readdir
    directory_entry_limit: 20000
    # how many entries to send on each readdir call (chunk size)
    directory_entry_send_amount: 500

  backups:
    # allow browsing backups via the web file manager
    mounting:
      # whether backup "mounting" is enabled
      enabled: true
      # what the start of the path should be for browsing
      # in this case, ".backups/<backup uuid>"
      path: .backups

    # settings for the wings backup driver
    wings:
      # what archive format to use for local (wings) backups
      # tar_gz, zip
      archive_format: tar_gz

    # settings for the ddup-bak backup driver
    ddup_bak:
      # how many threads to use when creating a ddup-bak backup
      create_threads: 4
      # the compression format to use for each ddup-bak chunk
      # none, deflate, gzip, brotli
      compression_format: deflate

    # settings for the btrfs backup driver
    btrfs:
      # how many threads to use when restoring a btrfs backup (snapshot)
      restore_threads: 4
      # whether to create the snapshots as read-only
      create_read_only: true

    # settings for the zfs backup driver
    zfs:
      # how many threads to use when restoring a zfs backup (snapshot)
      restore_threads: 4

docker:
  # the docker-compatible socket or http address to connect to
  socket: /var/run/docker.sock
  # whether to add (part) of the server name in the container name
  server_name_in_container_name: false

  network:
    # whether to disable binding to a specific ip
    disable_interface_binding: false
```

## added features

### api

- `GET /openapi.json` endpoint for getting a full OpenAPI documentation of the wings api
- `GET /api/stats` api endpoint for seeing node usage
- `GET /api/extensions` api endpoint for listing running extensions
- `GET /api/servers/{server}/version` api endpoint for getting a version hash for a server
- `GET /api/servers/{server}/files/fingerprints` api endpoint for getting fingerprints for many files at once
- `POST /api/servers/{server}/files/search` api endpoint for searching for file names/content
- `GET /api/servers/{server}/download/directory` api endpoint for downloading directories on-the-fly as `.tar.gz`s

---

- properly support egg `file_denylist`
- add support for `name` property on `POST /api/servers/{server}/files/copy`
- add support for opening individual compressed file (e.g. `.log.gz`) in `GET /api/servers/{server}/files/contents`
- add (real) folder size support on `GET /api/servers/{server}/files/list-directory`

### sftp

- add support for the [check-file](https://datatracker.ietf.org/doc/html/draft-ietf-secsh-filexfer-extensions-00#section-3) sftp extension
- add support for the [copy-file](https://datatracker.ietf.org/doc/html/draft-ietf-secsh-filexfer-extensions-00#section-6) sftp extension
- add support for the [space-available](https://datatracker.ietf.org/doc/html/draft-ietf-secsh-filexfer-extensions-00#section-4) sftp extension
- add support for the [limits@openssh.com](https://github.com/openssh/openssh-portable/blob/master/PROTOCOL#L597) sftp extension
- add support for the [statvfs@openssh.com](https://github.com/openssh/openssh-portable/blob/master/PROTOCOL#L510) sftp extension
- properly support egg `file_denylist`

### backups

- add [`ddup-bak`](https://github.com/0x7d8/ddup-bak) backup driver
- add [`btrfs`](https://github.com/kdave/btrfs-progs) backup driver
- add [`zfs`](https://github.com/openzfs/zfs) backup driver
- add ability to create `zip` archives on `wings` backup driver
- add ability to browse backups (for some drivers)
