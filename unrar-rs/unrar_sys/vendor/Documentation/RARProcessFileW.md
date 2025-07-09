<!DOCTYPE HTML PUBLIC "-//W3C//DTD HTML 4.01 Transitional//EN">
<html>

<head>
<title>UnRAR.dll Manual</title>
</head>

<body>

<h3>int PASCAL RARProcessFileW(HANDLE hArcData,int Operation,wchar_t *DestPath,wchar_t *DestName)</h3>

<h3>Description</h3>

<p>Performs the user defined action and moves the current position
in the archive to the next file.</p>
  
<p>If archive is opened in RAR_OM_EXTRACT mode, this function extracts
or tests the current file and sets the archive position to the next file.</p>

<p>If open mode is RAR_OM_LIST, then a call to this function will skip
the current file and set the archive position to the next file.</p>


<h3>Parameters</h3>

<i>hArcData</i>
<blockquote>
This parameter should contain the archive handle obtained from
<a href="RAROpenArchive.md">RAROpenArchive</a> or
<a href="RAROpenArchiveEx.md">RAROpenArchiveEx</a> function call.
</blockquote>

<i>Operation</i>
<blockquote>
  <p>File operation.</p>

  <p>Possible values</p>

  <table border="1">
  <tr>
    <td>RAR_SKIP</td>
    <td>Move to the next file in the archive. If archive is solid
        and RAR_OM_EXTRACT mode was set when the archive was opened,
        the current file is analyzed and operation is performed slower
        than a simple seek.</td>
  </tr><tr>
    <td>RAR_TEST</td>
    <td>Test the current file and move to the next file in the archive.
        If the archive was opened in RAR_OM_LIST mode, the operation
        is equal to RAR_SKIP.</td>
  </tr><tr>
    <td>RAR_EXTRACT</td>
    <td>Extract the current file and move to the next file in the archive.
        If the archive was opened with RAR_OM_LIST mode, the operation
        is equal to RAR_SKIP.</td>
  </tr>
  </table>
</blockquote>

<i>DestPath</i>
<blockquote>
  <p>This parameter should point to a zero terminated Unicode string,
  containing the destination directory to place the extracted files to.
  If DestPath is equal to NULL, it means extracting to the current directory.
  This parameter has meaning only if DestName is NULL.</p>
</blockquote>

<i>DestName</i>
<blockquote>
  <p>This parameter should point to a zero terminated Unicode string,
  containing the full path and name to assign to extracted file
  or it can be NULL to use the default name. If DestName is defined
  (not NULL), it overrides both the original file name stored
  in the archive and path specified in DestPath setting.</p>
</blockquote>


<h3>Return values</h3>
<blockquote>
<table border="1">
<tr><td> 0                   </td><td> Success </td></tr>
<tr><td> ERAR_BAD_DATA       </td><td> File CRC error </td></tr>
<tr><td> ERAR_UNKNOWN_FORMAT </td><td> Unknown archive format </td></tr>
<tr><td> ERAR_EOPEN          </td><td> Volume open error </td></tr>
<tr><td> ERAR_ECREATE        </td><td> File create error </td></tr>
<tr><td> ERAR_ECLOSE         </td><td> File close error </td></tr>
<tr><td> ERAR_EREAD          </td><td> Read error </td></tr>
<tr><td> ERAR_EWRITE         </td><td> Write error </td></tr>
<tr><td> ERAR_NO_MEMORY      </td><td> Not enough memory</td></tr>
<tr><td> ERAR_EREFERENCE     </td>
<td>When attempting to unpack a reference record (see RAR -oi switch),
source file for this reference was not found. Entire archive needs to be
unpacked to properly create file references. This error is returned when
attempting to unpack the reference record without its source file.</td></tr>
<tr><td> ERAR_BAD_PASSWORD    </td>
<td>Entered password is invalid. This code is returned only for archives
in RAR 5.0 format</td></tr>
</table>
</blockquote>

<h3>Notes</h3>
<blockquote>
  If you wish to cancel extraction, return -1 when processing UCM_PROCESSDATA
  message in <a href="RARCallback.md">user defined callback function</a>.
</blockquote>

</body>

</html>
