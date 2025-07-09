<!DOCTYPE HTML PUBLIC "-//W3C//DTD HTML 4.01 Transitional//EN">
<html>

<head>
<title>UnRAR.dll Manual</title>
</head>

<body>

<h3>int PASCAL RARCloseArchive(HANDLE hArcData)</h3>

<h3>Description</h3>
<p>
Close RAR archive and release allocated memory. It must be called when
archive processing is finished, even if the archive processing was stopped
due to an error.</p>

<h3>Parameters</h3>

<i>hArcData</i>
<blockquote>
This parameter should contain the archive handle obtained from
<a href="RAROpenArchive.md">RAROpenArchive</a> or
<a href="RAROpenArchiveEx.md">RAROpenArchiveEx</a> function call.
</blockquote>

<h3>Return values</h3>
<blockquote>
<table border="1">
<tr><td>0</td><td>Success</td></tr>
<tr><td>ERAR_ECLOSE</td><td>Archive close error</td></tr>
</table>
</blockquote>

<h3>See also</h3>
<blockquote>
  <a href="RAROpenArchiveEx.md">RAROpenArchiveEx</a> function.
</blockquote>

</body>

</html>
