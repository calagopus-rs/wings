use crate::error::*;
use crate::open_archive::{CursorBeforeHeader, List, ListSplit, OpenArchive, OpenMode, Process};
use regex::Regex;
use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

fn multipart_extension() -> &'static Regex {
    static INSTANCE: OnceLock<Regex> = OnceLock::new();
    INSTANCE.get_or_init(|| Regex::new(r"(\.part|\.r?)(\d+)((?:\.rar)?)$").unwrap())
}

fn extension() -> &'static Regex {
    static INSTANCE: OnceLock<Regex> = OnceLock::new();
    INSTANCE.get_or_init(|| Regex::new(r"(\.part|\.r?)(\d+)((?:\.rar)?)$|\.rar$").unwrap())
}

/// A RAR archive on the file system.
pub struct Archive<'a> {
    filename: Cow<'a, Path>,
    password: Option<&'a [u8]>,
    comments: Option<&'a mut Vec<u8>>,
}

pub type Glob = PathBuf;

impl<'a> Archive<'a> {
    /// Creates an `Archive` object to operate on a plain non-encrypted RAR archive.
    pub fn new<T>(file: &'a T) -> Self
    where
        T: AsRef<Path> + ?Sized,
    {
        Archive {
            filename: Cow::Borrowed(file.as_ref()),
            password: None,
            comments: None,
        }
    }

    /// Creates an `Archive` object to operate on a password encrypted RAR archive.
    pub fn with_password<F, Pw>(file: &'a F, password: &'a Pw) -> Self
    where
        F: AsRef<Path> + ?Sized,
        Pw: AsRef<[u8]> + ?Sized,
    {
        Archive {
            filename: Cow::Borrowed(file.as_ref()),
            password: Some(password.as_ref()),
            comments: None,
        }
    }

    /// Creates an `Archive` object to operate on a plain non-encrypted RAR archive.
    /// as opposed to [`new`](struct.Archive.html#method.new) that borrows from its input, this function takes ownership of it,
    /// potentially cloning if the input is a reference.
    /// this function is lifetime-free, allowing the creation of an `Archive<'static>`
    ///  with dynamically constructed Paths.
    pub fn new_owned<T>(file: T) -> Self
    where
        T: Into<PathBuf>,
    {
        Archive {
            filename: Cow::Owned(file.into()),
            password: None,
            comments: None,
        }
    }

    /// returns the archive's path
    pub fn filename(&self) -> &Path {
        &self.filename
    }

    /// Set the comment buffer of the underlying archive.
    /// Note: Comments are not supported yet so this method will have no effect.
    pub fn set_comments(&mut self, comments: &'a mut Vec<u8>) {
        self.comments = Some(comments);
    }

    /// Returns `true` if the filename matches a RAR archive.
    ///
    /// This method does not make any FS operations and operates purely on strings.
    pub fn is_archive(&self) -> bool {
        is_archive(&self.filename)
    }

    /// Returns `true` if the filename matches a part of a multipart collection, `false` otherwise
    ///
    /// This method does not make any FS operations and operates purely on strings.
    pub fn is_multipart(&self) -> bool {
        is_multipart(&self.filename)
    }

    /// Returns a glob string covering all parts of the multipart collection or `None` if the
    /// underlying archive does not appear to be a multipart archive (based solely on filename).
    ///
    /// This method does not make any FS operations and operates purely on strings.
    ///
    /// # Example
    ///
    /// Basic usage (multipart archive):
    ///
    /// ```
    /// # use unrar::Archive;
    /// # use std::path::PathBuf;
    /// let glob = Archive::new("path/my.archive.part01.rar").all_parts_option();
    ///
    /// assert_eq!(glob, Some(PathBuf::from("path/my.archive.part??.rar")));
    /// ```
    ///
    /// Single part archive:
    /// ```
    /// # use unrar::Archive;
    /// let glob = Archive::new("path/my.archive.rar").all_parts_option();
    ///
    /// assert_eq!(glob, None);
    /// ```
    pub fn all_parts_option(&self) -> Option<Glob> {
        get_rar_extension(&self.filename)
            .and_then(|full_ext| {
                multipart_extension().captures(&full_ext).map(|captures| {
                    let mut replacement = String::from(captures.get(1).unwrap().as_str());
                    replacement.push_str(
                        &std::iter::repeat_n("?", captures.get(2).unwrap().as_str().len())
                            .collect::<String>(),
                    );
                    replacement.push_str(captures.get(3).unwrap().as_str());
                    full_ext.replace(captures.get(0).unwrap().as_str(), &replacement)
                })
            })
            .and_then(|new_ext| {
                self.filename.file_stem().map(|x| {
                    self.filename
                        .with_file_name(Path::new(x).with_extension(&new_ext[1..]))
                })
            })
    }

    /// Returns a glob string covering all parts of the multipart collection or `self.filename` if
    /// the underlying archive does not appear to be a multipart archive (based solely on filename).
    ///
    /// This method does not make any FS operations and operates purely on strings.
    ///
    /// # Examples
    ///
    /// Basic usage (multipart archive):
    ///
    /// ```
    /// # use unrar::Archive;
    /// # use std::path::PathBuf;
    /// let glob = Archive::new("path/my.archive.part01.rar").all_parts();
    ///
    /// assert_eq!(glob, PathBuf::from("path/my.archive.part??.rar"));
    /// ```
    ///
    /// Single part archive:
    /// ```
    /// # use unrar::Archive;
    /// # use std::path::PathBuf;
    /// let glob = Archive::new("path/my.archive.rar").all_parts();
    ///
    /// assert_eq!(glob, PathBuf::from("path/my.archive.rar"));
    /// ```
    pub fn all_parts(&self) -> Glob {
        match self.all_parts_option() {
            Some(x) => x,
            None => self.filename.to_path_buf(),
        }
    }

    /// Returns the nth part of this multi-part collection or `None` if
    /// the underlying archive does not appear to be a multipart archive (based solely on filename).
    ///
    /// This method does not make any FS operations and operates purely on strings.
    ///
    /// # Examples
    ///
    /// Simple usage:
    ///
    /// ```
    /// # use unrar::Archive;
    /// # use std::path::PathBuf;
    /// let part42 = Archive::new("path/my.archive.part01.rar").nth_part(42).unwrap();
    ///
    /// assert_eq!(part42, PathBuf::from("path/my.archive.part42.rar"));
    /// ```
    ///
    /// Returns None for single-part archives:
    ///
    /// ```
    /// # use unrar::Archive;
    /// let part42 = Archive::new("path/my.archive.rar").nth_part(42);
    ///
    /// assert_eq!(part42, None);
    /// ```
    pub fn nth_part(&self, n: i32) -> Option<PathBuf> {
        get_rar_extension(&self.filename)
            .and_then(|full_ext| {
                multipart_extension().captures(&full_ext).map(|captures| {
                    let mut replacement = String::from(captures.get(1).unwrap().as_str());
                    // `n` padded with zeroes to the length of archive's number's length
                    replacement.push_str(&format!(
                        "{:01$}",
                        n,
                        captures.get(2).unwrap().as_str().len()
                    ));
                    replacement.push_str(captures.get(3).unwrap().as_str());
                    full_ext.replace(captures.get(0).unwrap().as_str(), &replacement)
                })
            })
            .and_then(|new_ext| {
                self.filename.file_stem().map(|x| {
                    self.filename
                        .with_file_name(Path::new(x).with_extension(&new_ext[1..]))
                })
            })
    }

    /// Return the first part of the multipart collection or `None` if
    /// the underlying archive does not appear to be a multipart archive (based solely on filename).
    ///
    /// This method does not make any FS operations and operates purely on strings.
    ///
    /// Equivalent to [`nth_part(1)`](Archive::nth_part).
    ///
    /// # Examples
    ///
    /// Simple usage:
    ///
    /// ```
    /// # use unrar::Archive;
    /// # use std::path::PathBuf;
    /// let part1 = Archive::new("path/my.archive.part42.rar").first_part_option().unwrap();
    ///
    /// assert_eq!(part1, PathBuf::from("path/my.archive.part01.rar"));
    /// ```
    ///
    /// Returns None for single-part archives:
    ///
    /// ```
    /// # use unrar::Archive;
    /// let part1 = Archive::new("path/my.archive.rar").first_part_option();
    ///
    /// assert_eq!(part1, None);
    /// ```
    pub fn first_part_option(&self) -> Option<PathBuf> {
        self.nth_part(1)
    }

    /// Returns the first part of the multipart collection or `self.filename` if
    /// the underlying archive does not appear to be a multipart archive (based solely on filename).
    ///
    /// This method does not make any FS operations and operates purely on strings.
    ///
    /// # Examples
    ///
    /// Basic usage:
    ///
    /// ```
    /// # use unrar::Archive;
    /// # use std::path::PathBuf;
    /// let part1 = Archive::new("path/archive.part33.rar").first_part();
    ///
    /// assert_eq!(part1, PathBuf::from("path/archive.part01.rar"));
    /// ```
    ///
    /// Single part archive:
    ///
    /// ```
    /// # use unrar::Archive;
    /// # use std::path::PathBuf;
    /// let part1 = Archive::new("path/archive.rar").first_part();
    ///
    /// assert_eq!(part1, PathBuf::from("path/archive.rar"));
    /// ```
    ///
    /// Note that this will always return the underlying path
    /// if a first part could not be found:
    ///
    /// ```
    /// # use unrar::Archive;
    /// # use std::path::PathBuf;
    /// let part1 = Archive::new("https://gibberish").first_part();
    ///
    /// assert_eq!(part1, PathBuf::from("https://gibberish"));
    /// ```
    pub fn first_part(&self) -> PathBuf {
        match self.nth_part(1) {
            Some(x) => x,
            None => self.filename.to_path_buf(),
        }
    }

    /// Changes the filename to point to the first part of the multipart collection. Does nothing if
    /// the underlying archive does not appear to be a multipart archive (based solely on filename).
    ///
    /// This method does not make any FS operations and operates purely on strings.
    ///
    /// # Example
    ///
    /// Basic usage:
    ///
    /// ```
    /// # use unrar::Archive;
    /// # use std::path::PathBuf;
    /// let mut archive = Archive::new("path/some.004.rar").as_first_part();
    /// assert_eq!(archive.filename(), PathBuf::from("path/some.001.rar"));
    /// ```
    pub fn as_first_part(mut self) -> Self {
        if let Some(fp) = self.first_part_option() {
            self.filename = Cow::Owned(fp);
        }
        self
    }

    /// Opens the underlying archive for processing, that is, the payloads of each archive entry can be
    /// actively read. What actually happens with individual entries (e.g. read, extract, skip, test),
    /// can be specified during processing.
    ///
    /// See also: [`Process`]
    ///
    /// # Panics
    ///
    /// Panics if `self.filename` contains nul values.
    pub fn open_for_processing(self) -> UnrarResult<OpenArchive<Process, CursorBeforeHeader>> {
        self.open(None)
    }

    /// Opens the underlying archive for listing its entries, i.e. the payloads are skipped automatically.
    ///
    /// See also: [`List`]
    ///
    /// # Panics
    ///
    /// Panics if `self.filename` contains nul values.
    pub fn open_for_listing(self) -> UnrarResult<OpenArchive<List, CursorBeforeHeader>> {
        self.open(None)
    }

    /// Opens the underlying archive for listing its entries without omitting or pooling split entries.
    /// For a multipart archive, this means a file spanning the border of 2 parts will appear twice,
    /// or even more often than that if it spans multiple parts, whereas in the normal list, it will
    /// appear once.
    ///
    /// See also: [`ListSplit`]
    ///
    /// # Panics
    ///
    /// Panics if `self.filename` contains nul values.
    pub fn open_for_listing_split(self) -> UnrarResult<OpenArchive<ListSplit, CursorBeforeHeader>> {
        self.open(None)
    }

    /// Opens the underlying archive with the provided parameters.
    ///
    /// # Panics
    ///
    /// Panics if `path` contains nul values.
    fn open<M: OpenMode>(
        self,
        recover: Option<&mut Option<OpenArchive<M, CursorBeforeHeader>>>,
    ) -> UnrarResult<OpenArchive<M, CursorBeforeHeader>> {
        OpenArchive::new(&self.filename, self.password, recover)
    }

    /// Opens the underlying archive with the provided OpenMode,
    /// even if archive is broken (e.g. malformed header).
    ///
    /// Provide an optional mutable reference for book-keeping, to check whether an error
    /// did occur. Note that this error will never be set if an Err is returned, i.e. if we
    /// were not able to read the archive.
    ///
    /// # Example: I don't care if there was a recoverable error
    ///
    /// ```no_run
    /// # use unrar::{Archive, List, UnrarResult};
    /// # fn x() -> UnrarResult<()> {
    /// let mut open_archive = Archive::new("file").break_open::<List>(None)?;
    /// // use open_archive
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Example: I want to know if there was a recoverable error
    ///
    /// ```no_run
    /// # use unrar::{Archive, List, UnrarResult};
    /// # fn x() -> UnrarResult<()> {
    /// let mut possible_error = None;
    /// let mut open_archive = Archive::new("file").break_open::<List>(Some(&mut possible_error))?;
    /// // check the error, e.g.:
    /// possible_error.map(|error| eprintln!("recoverable error occurred: {error}"));
    /// // use open_archive
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Panics
    ///
    /// Panics if `path` contains nul values.
    pub fn break_open<M: OpenMode>(
        self,
        error: Option<&mut Option<UnrarError>>,
    ) -> UnrarResult<OpenArchive<M, CursorBeforeHeader>> {
        let mut recovered = None;
        self.open(Some(&mut recovered))
            .or_else(|x| match recovered {
                Some(archive) => {
                    if let Some(error) = error {
                        *error = Some(x);
                    }
                    Ok(archive)
                }
                None => Err(x),
            })
    }
}

fn get_rar_extension<T: AsRef<Path>>(path: T) -> Option<String> {
    path.as_ref().extension().and_then(|ext| {
        let pre_ext = path
            .as_ref()
            .file_stem()
            .and_then(|x| Path::new(x).extension());
        Some(match pre_ext {
            Some(pre_ext) => format!(".{}.{}", pre_ext.to_str()?, ext.to_str()?),
            None => format!(".{}", ext.to_str()?),
        })
    })
}

pub fn is_archive(s: &Path) -> bool {
    get_rar_extension(s).is_some_and(|e| extension().is_match(&e))
}

pub fn is_multipart(s: &Path) -> bool {
    get_rar_extension(s).is_some_and(|e| multipart_extension().is_match(&e))
}

#[cfg(test)]
mod tests {
    use super::Archive;
    use std::path::PathBuf;

    #[test]
    fn glob() {
        assert_eq!(
            Archive::new("arc.part0010.rar").all_parts(),
            PathBuf::from("arc.part????.rar")
        );
        assert_eq!(
            Archive::new("archive.r100").all_parts(),
            PathBuf::from("archive.r???")
        );
        assert_eq!(
            Archive::new("archive.r9").all_parts(),
            PathBuf::from("archive.r?")
        );
        assert_eq!(
            Archive::new("archive.999").all_parts(),
            PathBuf::from("archive.???")
        );
        assert_eq!(
            Archive::new("archive.rar").all_parts(),
            PathBuf::from("archive.rar")
        );
        assert_eq!(
            Archive::new("random_string").all_parts(),
            PathBuf::from("random_string")
        );
        assert_eq!(
            Archive::new("v8/v8.rar").all_parts(),
            PathBuf::from("v8/v8.rar")
        );
        assert_eq!(Archive::new("v8/v8").all_parts(), PathBuf::from("v8/v8"));
    }

    #[test]
    fn first_part() {
        assert_eq!(
            Archive::new("arc.part0010.rar").first_part(),
            PathBuf::from("arc.part0001.rar")
        );
        assert_eq!(
            Archive::new("archive.r100").first_part(),
            PathBuf::from("archive.r001")
        );
        assert_eq!(
            Archive::new("archive.r9").first_part(),
            PathBuf::from("archive.r1")
        );
        assert_eq!(
            Archive::new("archive.999").first_part(),
            PathBuf::from("archive.001")
        );
        assert_eq!(
            Archive::new("archive.rar").first_part(),
            PathBuf::from("archive.rar")
        );
        assert_eq!(
            Archive::new("random_string").first_part(),
            PathBuf::from("random_string")
        );
        assert_eq!(
            Archive::new("v8/v8.rar").first_part(),
            PathBuf::from("v8/v8.rar")
        );
        assert_eq!(Archive::new("v8/v8").first_part(), PathBuf::from("v8/v8"));
    }

    #[test]
    fn is_archive() {
        assert_eq!(super::is_archive(&PathBuf::from("archive.rar")), true);
        assert_eq!(super::is_archive(&PathBuf::from("archive.part1.rar")), true);
        assert_eq!(
            super::is_archive(&PathBuf::from("archive.part100.rar")),
            true
        );
        assert_eq!(super::is_archive(&PathBuf::from("archive.r10")), true);
        assert_eq!(super::is_archive(&PathBuf::from("archive.part1rar")), false);
        assert_eq!(super::is_archive(&PathBuf::from("archive.rar\n")), false);
        assert_eq!(super::is_archive(&PathBuf::from("archive.zip")), false);
    }

    #[test]
    fn nul_in_input() {
        assert!(Archive::new("\0archive.rar").is_archive());
        assert!(Archive::with_password("archive.rar", "un\0rar").is_archive());
    }
}
