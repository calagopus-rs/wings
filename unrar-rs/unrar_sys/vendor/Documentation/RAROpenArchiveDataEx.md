<!DOCTYPE HTML PUBLIC "-//W3C//DTD HTML 4.01 Transitional//EN">
<html>

<head>
<title>UnRAR.dll Manual</title>
</head>

<body>

<h3>RAROpenArchiveDataEx structure</h3>

<pre>
struct RAROpenArchiveDataEx
{
  char         *ArcName;
  wchar_t      *ArcNameW;
  unsigned int  OpenMode;
  unsigned int  OpenResult;
  char         *CmtBuf;
  unsigned int  CmtBufSize;
  unsigned int  CmtSize;
  unsigned int  CmtState;
  unsigned int  Flags;
  UNRARCALLBACK Callback;
  LPARAM        UserData;
  unsigned int  OpFlags;
  wchar_t      *CmtBufW;
  unsigned int  Reserved[25];
};
</pre>

<h3>Description</h3>
<p>This structure is used by <a href="RAROpenArchiveEx.md">RAROpenArchiveEx</a>
function.</p>

<h3>Structure fields</h3>

<ul>
<li>
<i>ArcName</i>
<blockquote>
  Input parameter which should point to zero terminated string 
  containing the archive name or NULL if only Unicode name is specified.
</blockquote>

<li>
<i>ArcNameW</i>
<blockquote>
  Input parameter which should point to zero terminated Unicode string
  containing the archive name or NULL if Unicode name is not specified.
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
  <p>Input parameter which should point to buffer for archive comment.
  Comment text is zero terminated. If comment text is larger than 
  buffer size, comment text is truncated.</p>
  
  <p>Comment is read in a native single byte encoding. If you need Unicode
  comment, use CmtBufW.</p>

  <p>Set to NULL if you do not need to read non-Unicode comment.</p>
</blockquote>

<li>
<i>CmtBufSize</i>
<blockquote>
  Input parameter which should contain a size of buffer for archive comments.
  Used either with CmtBuf or CmtBufW. Size is specified in characters,
  which are a single byte for CmtBuf or wchar_t size for CmtBufW.
</blockquote>

<li>
<i>CmtSize</i>
<blockquote>
  Output parameter containing a size of comment actually read into the buffer,
  cannot exceed CmtBufSize.
  Used either with CmtBuf or CmtBufW. Size is returned in characters,
  which are a single byte for CmtBuf or wchar_t size for CmtBufW.
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

<li>
<i>Flags</i>
<blockquote>
  <p>Output parameter. Combination of bit flags.</p>

  <p>Possible values:</p>

  <table border=1>
  <tr><td>ROADF_VOLUME</td><td> 0x0001 </td><td> Volume attribute (archive volume)</td></tr>
  <tr><td>ROADF_COMMENT</td><td> 0x0002 </td><td> Archive comment present</td></tr>
  <tr><td>ROADF_LOCK</td><td> 0x0004 </td><td> Archive lock attribute</td></tr>
  <tr><td>ROADF_SOLID</td><td> 0x0008 </td><td> Solid attribute (solid archive)</td></tr>
  <tr><td>ROADF_NEWNUMBERING</td><td> 0x0010 </td><td> New volume naming scheme ('volname.partN.rar')</td></tr>
  <tr><td>ROADF_SIGNED</td><td> 0x0020 </td><td> Authenticity information present (obsolete)</td></tr>
  <tr><td>ROADF_RECOVERY</td><td> 0x0040 </td><td> Recovery record present</td></tr>
  <tr><td>ROADF_ENCHEADERS</td><td> 0x0080 </td><td> Block headers are encrypted</td></tr>
  <tr><td>ROADF_FIRSTVOLUME</td><td> 0x0100 </td><td> First volume (set only by RAR 3.0 and later)</td></tr>
  </table>
</blockquote>

<li>
<i>Callback</i>
<blockquote>
  <p>Address of <a href="RARCallback.md">user defined callback function</a>
  to process UnRAR events.</p>
  <p>Set it to NULL if you do not want to define the callback function.
  Callback function is required to process multivolume and encrypted
  archives properly.</p>
</blockquote>

<li>
<i>UserData</i>
<blockquote>
  <p>User defined value, which will be passed to 
  <a href="RARCallback.md">callback function.</a></p>
</blockquote>

<li>
<i>OpFlags</i>
<blockquote>
  <p>Input parameter. Operation mode modifiers. Combination of bit flags.</p>

  <p>Possible values:</p>

  <table border=1>
  <tr><td>ROADOF_KEEPBROKEN</td><td> 0x0001 </td><td>If set, extracted files with checksum errors are not deleted</td></tr>
  </table>
</blockquote>

<li>
<i>CmtBufW</i>
<blockquote>
  <p>Input parameter which should point to buffer for archive comment
  in Unicode encoding. Comment text is zero terminated. If comment text
  is larger than buffer size, comment text is truncated.</p>

  <p>Set to NULL if you do not need to read Unicode comment.</p>

  <p>If both CmtBuf and CmtBufW are not NULL, only CmtBufW comment is read.
  If both CmtBuf and CmtBufW are NULL, archive comment is not read.</p>
</blockquote>

<li>
<i>Reserved[25]</i>
<blockquote>
  <p>Reserved for future use. Must be zero.</p>
</blockquote>

</ul>

<h3>See also</h3>
<blockquote>
  <a href="RAROpenArchive.md">RAROpenArchiveEx</a> function.<br>
  <a href="RARCallback.md">User defined callback function</a>
</blockquote>

</body>

</html>
