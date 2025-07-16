# pterodactyl-rs wings (experimental)

a rewrite of [pterodactyl wings](https://github.com/pterodactyl/wings) in the rust programming language. this rewrite aims to be 100% API compatible while implementing new features and better performance.

## quick installation

### please remember to update atleast weekly

```bash
sudo curl -L "https://github.com/pterodactyl-rs/wings/releases/latest/download/wings-rs-$(uname -m)-linux$(ldd --version 2>&1 | grep -q 'GLIBC 2.3[5-9]\|GLIBC 2.[4-9]' || echo '-musl')" -o /usr/local/bin/wings
sudo chmod +x /usr/local/bin/wings

wings version
```

## added config options

```yml
api:
  # cidrs to block on the remote download pull endpoint
  remote_download_blocked_cidrs:
  - '127.0.0.0/8'
  - '10.0.0.0/8'
  - '172.16.0.0/12'
  - '192.168.0.0/16'
  - '169.254.0.0/16'
  - ::1
  - fe80::/10
  - fc00::/7
  # whether to disable the /openapi.json endpoint
  disable_openapi_docs: false
  # how many entries can be listed on a single page on the /list-directory API call, 0 means unlimited
  directory_entry_limit: 10000
  # send server logs of an offline server when connecting to ws
  send_offline_server_logs: false
  # how many threads to use when searching files using file search
  file_search_threads: 4
  # how many threads to use when decompressing .zip/.7z/.ddup
  file_decompression_threads: 2

system:
  # apply a real quota limit to each server
  # none, btrfs_subvolume, zfs_dataset, xfs_quota
  disk_limiter_mode: none

  sftp:
    # the algorithm to use for the ssh host key
    key_algorithm: ssh-ed25519
    # whether to disable password auth for the sftp server
    disable_password_auth: false
    # how many entries can be listed on readdir, 0 means unlimited
    directory_entry_limit: 20000
    # how many entries to send on each readdir call (chunk size)
    directory_entry_send_amount: 500

    shell:
      # whether to enable the wings remote shell (allows server management over ssh)
      enabled: true

      cli:
        # what to call the internal cli for managing server actions (e.g. ".wings help")
        name: ".wings"

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
      # how many threads to use when restoring a zip wings backup
      restore_threads: 4
      # what archive format to use for local (wings) backups
      # tar, tar_gz, tar_zstd, zip
      archive_format: tar_gz

    # settings for the ddup-bak backup driver
    ddup_bak:
      # how many threads to use when creating a ddup-bak backup
      create_threads: 4
      # the compression format to use for each ddup-bak chunk
      # none, deflate, gzip, brotli
      compression_format: deflate

    # settings for the restic backup driver
    restic:
      # the repository to use for restic backups (must already be initialized)
      repository: /var/lib/pterodactyl/backups/restic
      # the password file to use for authenticating against the repository
      password-file: /var/lib/pterodactyl/backups/restic_password
      # how long to wait for a repository lock if locked in seconds
      retry_lock_seconds: 60
      # the restic cli environment for each command (useful for s3 credentials, etc)
      environment: {}

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

  installer_limits:
    # how long in seconds to wait until an install container is considered failed, 0 means no limit
    timeout_seconds: 1800
```

## added features

### api

- `GET /openapi.json` endpoint for getting a full OpenAPI documentation of the wings api
- `GET /api/stats` api endpoint for seeing node usage
- `POST /api/servers/{server}/script` api endpoint for running custom scripts async to the server
- `POST /api/servers/{server}/ws/permissions` api endpoint for live updating user permissions on a server
- `GET /api/servers/{server}/version` api endpoint for getting a version hash for a server
- `GET /api/servers/{server}/files/fingerprints` api endpoint for getting fingerprints for many files at once
- `GET /api/servers/{server}/files/list` api endpoint for listing files with pagination
- `POST /api/servers/{server}/files/search` api endpoint for searching for file names/content
- `GET /api/servers/{server}/download/directory` api endpoint for downloading directories on-the-fly as `.tar.gz`s

---

- properly support egg `file_denylist`
- add support for `name` property on `POST /api/servers/{server}/files/copy`
- add support for opening individual compressed file (e.g. `.log.gz`) in `GET /api/servers/{server}/files/contents`
- add (real) folder size support on `GET /api/servers/{server}/files/list-directory`
- add multithreading support to `POST /api/servers/{server}/files/decompress`
- add support for `ignored_files` in the file upload jwt
- allow transferring backups in server transfers

### shell

- add ability to connect via ssh and access server console
- add `.wings` cli to do basic server actions like power

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
- add [`restic`](https://github.com/restic/restic) backup driver
- add ability to create `zip` archives on `wings` backup driver
- add ability to browse backups (for some drivers)
