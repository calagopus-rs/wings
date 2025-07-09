<!DOCTYPE HTML PUBLIC "-//W3C//DTD HTML 4.01 Transitional//EN">
<html>

<head>
<title>UnRAR.dll Manual</title>
</head>

<body>

<h3>RARHeaderData structure</h3>

<pre>
struct RARHeaderData
{
  char         ArcName[260];
  char         FileName[260];
  unsigned int Flags;
  unsigned int PackSize;
  unsigned int UnpSize;
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
};
</pre>

<h3>Description</h3>
<p>This structure is used by <a href="RARReadHeader.md">RARReadHeader</a>
function.</p>

<h3>Structure fields</h3>

<ul>
<li>
<i>ArcName</i>
<blockquote>
  Output parameter which contains a zero terminated string of the
  current archive name. May be used to determine the current volume name. 
</blockquote>

<li>
<i>FileName</i>
<blockquote>
  Output parameter which contains a zero terminated string of the 
  file name in OEM (DOS) encoding.
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
  <p>Other bits are reserved.</p>
</blockquote>

<li>
<i>PackSize</i>
<blockquote>
  Output parameter. Packed file size or size of file part if file
  was split between volumes.
</blockquote>

<li>
<i>UnpSize</i>
<blockquote>
  Output parameter. Unpacked file size.
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
  <p>Output parameter. Size of file comment read into buffer.<p>
  <p>File comment support is not implemented in current unrar.dll version.</p>
  <p>Always equal to 0.</p>
</blockquote>

<li>
<i>CmtState</i>
<blockquote>
  <p>Output parameter. State of file comment.<p>
  <p>File comment support is not implemented in current unrar.dll version.</p>
  <p>Always equal to 0.</p>
</blockquote>

</ul>

<h3>See also</h3>
<blockquote>
  <a href="RARReadHeader.md">RARReadHeader</a> function.
</blockquote>

</body>

</html>
