use std::path::PathBuf;

pub struct AsyncWalkDir {
    server: crate::server::Server,
    stack: Vec<(PathBuf, super::AsyncReadDir)>,
}

impl AsyncWalkDir {
    pub async fn new(server: crate::server::Server, path: PathBuf) -> Result<Self, anyhow::Error> {
        let read_dir = server.filesystem.read_dir(&path).await?;

        Ok(Self {
            server,
            stack: vec![(path, read_dir)],
        })
    }

    pub async fn next_entry(&mut self) -> Option<Result<(bool, PathBuf), anyhow::Error>> {
        while let Some((path, read_dir)) = self.stack.last_mut() {
            match read_dir.next_entry().await {
                Some(Ok((is_dir, name))) => {
                    let path = path.join(name);

                    if is_dir {
                        let new_read_dir = match self.server.filesystem.read_dir(&path).await {
                            Ok(dir) => dir,
                            Err(e) => return Some(Err(e)),
                        };
                        self.stack.push((path.clone(), new_read_dir));
                    }

                    return Some(Ok((is_dir, path)));
                }
                Some(Err(err)) => return Some(Err(err.into())),
                None => {
                    self.stack.pop();
                }
            }
        }

        None
    }
}

pub struct WalkDir {
    server: crate::server::Server,
    stack: Vec<(PathBuf, super::ReadDir)>,
}

impl WalkDir {
    pub fn new(server: crate::server::Server, path: PathBuf) -> Result<Self, anyhow::Error> {
        let read_dir = server.filesystem.read_dir_sync(&path)?;

        Ok(Self {
            server,
            stack: vec![(path, read_dir)],
        })
    }

    pub fn next_entry(&mut self) -> Option<Result<(bool, PathBuf), anyhow::Error>> {
        while let Some((path, read_dir)) = self.stack.last_mut() {
            match read_dir.next_entry() {
                Some(Ok((is_dir, name))) => {
                    let path = path.join(name);

                    if is_dir {
                        let new_read_dir = match self.server.filesystem.read_dir_sync(&path) {
                            Ok(dir) => dir,
                            Err(e) => return Some(Err(e)),
                        };
                        self.stack.push((path.clone(), new_read_dir));
                    }

                    return Some(Ok((is_dir, path)));
                }
                Some(Err(err)) => return Some(Err(err.into())),
                None => {
                    self.stack.pop();
                }
            }
        }

        None
    }
}
