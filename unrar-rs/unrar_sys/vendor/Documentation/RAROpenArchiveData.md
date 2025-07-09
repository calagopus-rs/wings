<!DOCTYPE HTML PUBLIC "-//W3C//DTD HTML 4.01 Transitional//EN">
<html>

<head>
<title>UnRAR.dll Manual</title>
</head>

<body>

<h3>RAROpenArchiveData structure</h3>

<pre>
struct RAROpenArchiveData
{
  char *ArcName;
  UINT OpenMode;
  UINT OpenResult;
  char *CmtBuf;
  UINT CmtBufSize;
  UINT CmtSize;
  UINT CmtState;
};
</pre>

<h3>Description</h3>
<p>This structure is used by <a href="RAROpenArchive.md">RAROpenArchive</a>
function.</p>

<h3>Structure fields</h3>

<ul>
<li>
<i>ArcName</i>
<blockquote>
  Input parameter which should point to zero terminated string 
  containing the archive name.
</blockquote>

<li>
<i>OpenMode</i>
<blockquote>
  <p>Input parameter.</p>

  <p>Possible values</p>

  <ul>
  <li>
  <b>RAR_OM_LIST</b>
    <p>Open archive for reading file headers only.</p>

  <li>
  <b>RAR_OM_EXTRACT</b>
    <p>Open archive for testing and extracting files.</p>

  <li>
  <b>RAR_OM_LIST_INCSPLIT</b>
    <p>Open archive for reading file headers only. If you open an archive
    in such mode, RARReadHeader[Ex] will return all file headers,
    including those with "file continued from previous volume" flag.
    In case of RAR_OM_LIST such headers are automatically skipped.
    So if you process RAR volumes in RAR_OM_LIST_INCSPLIT mode, you will
    get several file header records for same file if file is split between
    volumes. For such files only the last file header record will contain
    the correct file CRC and if you wish to get the correct packed size,
    you need to sum up packed sizes of all parts.</p>
  </ul>
</blockquote>

<li>
<i>OpenResult</i>
<blockquote>
  <p>Output parameter.</p>

  <p>Possible values:</p>

  <table border=1>
  <tr><td> 0                   </td><td> Success</td></tr>
  <tr><td> ERAR_NO_MEMORY      </td><td> Not enough memory to initialize data structures</td></tr>
  <tr><td> ERAR_BAD_DATA       </td><td> Archive header broken</td></tr>
  <tr><td> ERAR_UNKNOWN_FORMAT </td><td> Unknown encryption used for archive headers </td></tr>
  <tr><td> ERAR_EOPEN          </td><td> File open error</td></tr>
  <tr><td> ERAR_BAD_PASSWORD    </td>
  <td>Entered password is invalid. This code is returned only for archives
  in RAR 5.0 format</td></tr>
  </table>

</blockquote>

<li>
<i>CmtBuf</i>
<blockquote>
  Input parameter which should point to the buffer for archive 
  comments. Maximum comment size is limited to 64Kb. Comment text is 
  zero terminated. If the comment text is larger than the buffer 
  size, the comment text will be truncated. If CmtBuf is set to 
  NULL, comments will not be read.
</blockquote>

<li>
<i>CmtBufSize</i>
<blockquote>
  Input parameter which should contain size of buffer for archive
  comments.
</blockquote>

<li>
<i>CmtSize</i>
<blockquote>
  Output parameter containing size of comments actually read into the
  buffer, cannot exceed CmtBufSize.
</blockquote>

<li>
<i>CmtState</i>
<blockquote>
  <p>Output parameter.</p>

  <p>Possible values:</p>

  <table border=1>
  <tr><td> 0              </td><td> Comments are not present</td></tr>
  <tr><td> 1              </td><td> Comments are read completely</td></tr>
  <tr><td> ERAR_NO_MEMORY </td><td> Not enough memory to extract comments</td></tr>
  <tr><td> ERAR_BAD_DATA  </td><td> Broken comment</td></tr>
  <tr><td> ERAR_SMALL_BUF </td><td> Buffer is too small, comments are not read completely.</td></tr>
  </table>
</blockquote>
</ul>

<h3>See also</h3>
<blockquote>
  <a href="RAROpenArchive.md">RAROpenArchive</a> function.
</blockquote>

</body>

</html>
