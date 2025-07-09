<!DOCTYPE HTML PUBLIC "-//W3C//DTD HTML 4.01 Transitional//EN">
<html>

<head>
<title>UnRAR.dll Manual</title>
</head>

<body>

<h3>int PASCAL RARReadHeaderEx(HANDLE hArcData,struct RARHeaderDataEx *HeaderData)</h3>

<h3>Description</h3>
<p>Read a file header from archive.</p>


<h3>Parameters</h3>

<i>hArcData</i>
<blockquote>
This parameter should contain the archive handle obtained from
<a href="RAROpenArchive.md">RAROpenArchive</a> or
<a href="RAROpenArchiveEx.md">RAROpenArchiveEx</a> function call.
</blockquote>

<i>HeaderData</i>
<blockquote>
Points to <a href="RARHeaderDataEx.md">RARHeaderDataEx structure</a>.
</blockquote>

<h3>Return values</h3>
<blockquote>
<table border="1">
<tr><td>0</td><td>Success</td></tr>
<tr><td>ERAR_END_ARCHIVE</td><td>End of archive</td></tr>
<tr><td>ERAR_BAD_DATA</td><td>File header broken</td></tr>
<tr><td>ERAR_MISSING_PASSWORD</td><td>Password was not provided
for encrypted file header</td></tr>
<tr><td>ERAR_EOPEN</td><td> Volume open error </td></tr>
</table>
</blockquote>

<h3>See also</h3>
<blockquote>
  <a href="RARHeaderDataEx.md">RARHeaderDataEx</a> structure.
</blockquote>

</body>

</html>
