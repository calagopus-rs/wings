use std::{path::PathBuf, sync::Arc};
use tokio::sync::Semaphore;

pub struct AsyncWalkDir<'a> {
    server: crate::server::Server,
    stack: Vec<(PathBuf, super::AsyncReadDir)>,
    ignored: &'a [ignore::gitignore::Gitignore],
}

impl<'a> AsyncWalkDir<'a> {
    pub async fn new(server: crate::server::Server, path: PathBuf) -> Result<Self, anyhow::Error> {
        let read_dir = server.filesystem.read_dir(&path).await?;

        Ok(Self {
            server,
            stack: vec![(path, read_dir)],
            ignored: &[],
        })
    }

    pub fn with_ignored(mut self, ignored: &'a [ignore::gitignore::Gitignore]) -> Self {
        self.ignored = ignored;
        self
    }

    pub async fn next_entry(&mut self) -> Option<Result<(bool, PathBuf), anyhow::Error>> {
        'stack: while let Some((parent_path, read_dir)) = self.stack.last_mut() {
            match read_dir.next_entry().await {
                Some(Ok((is_dir, name))) => {
                    let full_path = parent_path.join(&name);

                    let should_ignore = self
                        .ignored
                        .iter()
                        .any(|ignored| ignored.matched(&full_path, is_dir).is_ignore());
                    if should_ignore {
                        continue 'stack;
                    }

                    if is_dir {
                        match self.server.filesystem.read_dir(&full_path).await {
                            Ok(dir) => self.stack.push((full_path.clone(), dir)),
                            Err(e) => return Some(Err(e)),
                        };
                    }

                    return Some(Ok((is_dir, full_path)));
                }
                Some(Err(err)) => return Some(Err(err.into())),
                None => {
                    self.stack.pop();
                }
            }
        }

        None
    }

    pub async fn run_multithreaded<F, Fut>(
        &mut self,
        threads: usize,
        func: Arc<F>,
    ) -> Result<(), anyhow::Error>
    where
        F: Fn(bool, PathBuf) -> Fut + Send + Sync + 'static,
        Fut: futures::Future<Output = ()> + Send + 'static,
    {
        let semaphore = Arc::new(Semaphore::new(threads));

        while let Some(entry) = self.next_entry().await {
            match entry {
                Ok((is_dir, path)) => {
                    let semaphore = Arc::clone(&semaphore);
                    let func = Arc::clone(&func);

                    let permit = match semaphore.acquire_owned().await {
                        Ok(permit) => permit,
                        Err(_) => break,
                    };
                    tokio::spawn(async move {
                        let _permit = permit;
                        func(is_dir, path).await;
                    });
                }
                Err(err) => return Err(err),
            }
        }

        semaphore.acquire_many(threads as u32).await.ok();

        Ok(())
    }
}

pub struct WalkDir<'a> {
    server: crate::server::Server,
    stack: Vec<(PathBuf, super::ReadDir)>,
    ignored: &'a [ignore::gitignore::Gitignore],
}

impl<'a> WalkDir<'a> {
    pub fn new(server: crate::server::Server, path: PathBuf) -> Result<Self, anyhow::Error> {
        let read_dir = server.filesystem.read_dir_sync(&path)?;

        Ok(Self {
            server,
            stack: vec![(path, read_dir)],
            ignored: &[],
        })
    }

    pub fn with_ignored(mut self, ignored: &'a [ignore::gitignore::Gitignore]) -> Self {
        self.ignored = ignored;
        self
    }

    pub fn next_entry(&mut self) -> Option<Result<(bool, PathBuf), anyhow::Error>> {
        'stack: while let Some((parent_path, read_dir)) = self.stack.last_mut() {
            match read_dir.next_entry() {
                Some(Ok((is_dir, name))) => {
                    let full_path = parent_path.join(&name);

                    let should_ignore = self
                        .ignored
                        .iter()
                        .any(|ignored| ignored.matched(&full_path, is_dir).is_ignore());
                    if should_ignore {
                        continue 'stack;
                    }

                    if is_dir {
                        match self.server.filesystem.read_dir_sync(&full_path) {
                            Ok(dir) => self.stack.push((full_path.clone(), dir)),
                            Err(e) => return Some(Err(e)),
                        };
                    }

                    return Some(Ok((is_dir, full_path)));
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
