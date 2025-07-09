<!DOCTYPE HTML PUBLIC "-//W3C//DTD HTML 4.01 Transitional//EN">
<html>

<head>
<title>UnRAR.dll Manual</title>
</head>

<body>

<h3>RARHeaderDataEx structure</h3>

<pre>
struct RARHeaderDataEx
{
  char         ArcName[1024];
  wchar_t      ArcNameW[1024];
  char         FileName[1024];
  wchar_t      FileNameW[1024];
  unsigned int Flags;
  unsigned int PackSize;
  unsigned int PackSizeHigh;
  unsigned int UnpSize;
  unsigned int UnpSizeHigh;
  unsigned int HostOS;
  unsigned int FileCRC;
  unsigned int FileTime;
  unsigned int UnpVer;
  unsigned int Method;
  unsigned int FileAttr;
  char         *CmtBuf;
  unsigned int CmtBufSize;
  unsigned int CmtSize;
  unsigned int CmtState;
  unsigned int DictSize;
  unsigned int HashType;
  char         Hash[32];
  unsigned int RedirType;
  wchar_t      *RedirName;
  unsigned int RedirNameSize;
  unsigned int DirTarget;
  unsigned int MtimeLow;
  unsigned int MtimeHigh;
  unsigned int CtimeLow;
  unsigned int CtimeHigh;
  unsigned int AtimeLow;
  unsigned int AtimeHigh;
  unsigned int Reserved[988];
};
</pre>

<h3>Description</h3>
<p>This structure is used by <a href="RARReadHeaderEx.md">RARReadHeaderEx</a>
function. Please fill either the entire structure or at least its Reserved
field with zeroes before passing to RARReadHeaderEx.</p>

<h3>Structure fields</h3>

<ul>
<li>
<i>ArcName</i>
<blockquote>
  Output parameter, which contains a zero terminated string of the current
  archive name. May be used to determine the current volume name. 
</blockquote>

<li>
<i>ArcNameW</i>
<blockquote>
  Output parameter, which contains a zero terminated string of the current
  archive name in Unicode. May be used to determine the current volume name. 
</blockquote>

<li>
<i>FileName</i>
<blockquote>
  Output parameter, which contains a zero terminated string of the file name
  in OEM (DOS) encoding.
</blockquote>

<li>
<i>FileNameW</i>
<blockquote>
  Output parameter, which contains a zero terminated string of the file name
  in Unicode.
</blockquote>

<li>
<i>Flags</i>
<blockquote>
  <p>Output parameter which contains file flags:</p>
  <table border="1">
  <tr><td>RHDF_SPLITBEFORE</td><td>0x01</td><td>File continued from previous volume</td></tr>
  <tr><td>RHDF_SPLITAFTER</td><td>0x02</td><td>File continued on next volume</td></tr>
  <tr><td>RHDF_ENCRYPTED</td><td>0x04</td><td>File encrypted with password</td></tr>
  <tr><td></td><td>0x08</td><td>Reserved</td></tr>
  <tr><td>RHDF_SOLID</td><td>0x10</td><td>Previous files data is used (solid flag)</td></tr>
  <tr><td>RHDF_DIRECTORY</td><td>0x20</td><td>Directory entry</td></tr>
  </table>
</blockquote>

<li>
<i>PackSize</i>
<blockquote>
  Output parameter. Lower 32 bits of packed file size. If file is split
  between volumes, it contains the lower 32 bits of file part size.
</blockquote>

<li>
<i>PackSizeHigh</i>
<blockquote>
  Output parameter. Higher 32 bits of packed file size. If file is split
  between volumes, it contains the higher 32 bits of file part size.
</blockquote>

<li>
<i>UnpSize</i>
<blockquote>
  Output parameter. Lower 32 bits of unpacked file size.
</blockquote>

<li>
<i>UnpSizeHigh</i>
<blockquote>
  Output parameter. Higher 32 bits of unpacked file size.
</blockquote>

<li>
<i>HostOS</i>
<blockquote>
  <p>Output parameter. Operating system used to create the archive.</p>

  <table border="1">
  <tr><td>0</td><td>MS DOS</td></tr>
  <tr><td>1</td><td>OS/2</td></tr>
  <tr><td>2</td><td>Windows</td></tr>
  <tr><td>3</td><td>Unix</td></tr>
  </table>
</blockquote>

<li>
<i>FileCRC</i>
<blockquote>
  Output parameter, which contains unpacked file CRC32. In case of file
  parts split between volumes only the last part contains the correct
  CRC and it is accessible only in RAR_OM_LIST_INCSPLIT listing mode.
</blockquote>

<li>
<i>FileTime</i>
<blockquote>
  Output parameter. Contains the file modification date and time in standard
  MS DOS format.
</blockquote>

<li>
<i>UnpVer</i>
<blockquote>
  Output parameter. RAR version needed to extract file.
  It is encoded as 10 * Major version + minor version.
</blockquote>

<li>
<i>Method</i>
<blockquote>
  <p>Output parameter. Packing method.</p>

  <table border="1">
  <tr><td>0x30</td><td>Storing</td></tr>
  <tr><td>0x31</td><td>Fastest compression</td></tr>
  <tr><td>0x32</td><td>Fast compression</td></tr>
  <tr><td>0x33</td><td>Normal compression</td></tr>
  <tr><td>0x34</td><td>Good compression</td></tr>
  <tr><td>0x35</td><td>Best compression</td></tr>
  </table>

</blockquote>

<li>
<i>FileAttr</i>
<blockquote>
  Output parameter. File attributes.
</blockquote>
</ul>

<li>
<i>CmtBuf</i>
<blockquote>
  <p>Input parameter. Points to the buffer for file comment.</p>
  <p>File comment support is not implemented in current unrar.dll version.
  Appropriate parameters are preserved only for compatibility
  with older versions.</p>
  <p>Set this field to NULL.</p>
</blockquote>

<li>
<i>CmtBufSize</i>
<blockquote>
  <p>Input parameter. Size of buffer for file comments.</p>
  <p>File comment support is not implemented in current unrar.dll version.</p>
  <p>Set this field to 0.</p>
</blockquote>

<li>
<i>CmtSize</i>
<blockquote>
  <p>Output parameter. Size of file comment read into buffer.</p>
  <p>File comment support is not implemented in current unrar.dll version.</p>
  <p>Always equal to 0.</p>
</blockquote>

<li>
<i>CmtState</i>
<blockquote>
  <p>Output parameter. State of file comment.</p>
  <p>File comment support is not implemented in current unrar.dll version.</p>
  <p>Always equal to 0.</p>
</blockquote>

<li>
<i>DictSize</i>
<blockquote>
  <p>Output parameter. Size of file compression dictionary in kilobytes.
</blockquote>

<li>
<i>HashType</i>
<blockquote>
  <p>Output parameter. Type of hash function used to protect file data
  integrity. Can be RAR_HASH_NONE (no checksum or unknown hash function type),
  RAR_HASH_CRC32 (CRC32) or RAR_HASH_BLAKE2 (BLAKE2sp)</p>
</blockquote>

<li>
<i>Hash</i>
<blockquote>
  <p>Output parameter. If HashType is equal to RAR_HASH_BLAKE2, this array
  contains 32 bytes of file data BLAKE2sp hash.</p>
</blockquote>

<li>
<i>RedirType</i>
<blockquote>
  <p>Output parameter. Type of file system redirection.</p>
  <table border="1">
  <tr><td>0</td><td>No redirection, usual file.</td></tr>
  <tr><td>1</td><td>Unix symbolic link</td></tr>
  <tr><td>2</td><td>Windows symbolic link</td></tr>
  <tr><td>3</td><td>Windows junction</td></tr>
  <tr><td>4</td><td>Hard link</td></tr>
  <tr><td>5</td><td>File reference saved with -oi switch</td></tr>
  </table>

</blockquote>

<li>
<i>RedirName</i>
<blockquote>
  <p>Input/output parameter. Pointer to buffer to receive file system
  redirection target name, such as target of symbolic link or file reference.
  It is returned as stored in archive and its value might be not immediately
  applicable for further use. For example, you may need to remove \??\
  or UNC\ prefixes for Windows junctions or prepend the extraction
  destination path.</p>
  <p>If you set RedirName to NULL, it is ignored and nothing is returned
  here.</p>
</blockquote>

<li>
<i>RedirNameSize</i>
<blockquote>
  <p>Input parameter. Size of buffer specified in RedirName. Ignored if
  RedirName is NULL.</p>
</blockquote>

<li>
<i>DirTarget</i>
<blockquote>
  <p>Output parameter. Non-zero if RedirType is symbolic link
  and RedirName points to directory.</p>
</blockquote>

<li>
<i>MtimeLow, MtimeHigh, CtimeLow, CtimeHigh, AtimeLow, AtimeHigh</i>
<blockquote>
  <p>Output parameters. Low and high 32 bit values of file modification,
  creation and last access time in Windows FILETIME format
  in Coordinated Universal Time (UTC). If appropriate file time is not
  stored in archive, both low and high values are set to 0.</p>
</blockquote>

<li>
<i>Reserved</i>
<blockquote>
  Reserved for future use. The entire array must be filled with zeroes
  before passing RARHeaderDataEx structure to RARReadHeaderEx.
</blockquote>

</ul>

<h3>See also</h3>
<blockquote>
  <a href="RARReadHeaderEx.md">RARReadHeaderEx</a> function.
</blockquote>

</body>

</html>
