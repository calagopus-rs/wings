<!DOCTYPE HTML PUBLIC "-//W3C//DTD HTML 4.01 Transitional//EN">
<html>

<head>
<title>UnRAR.dll Manual</title>
</head>

<body>

<h3>void PASCAL RARSetCallback(HANDLE hArcData,
int PASCAL (*Callback)(UINT msg,LPARAM UserData,LPARAM P1,LPARAM P2),
LPARAM UserData)</h3>

<h3>Description</h3>

<p>Set a <a href="RARCallback.md">user defined callback function</a>
to process UnRAR events.</p>

<p>RARSetCallback is obsolete and less preferable way to specify the callback
function. Recommended approach is to set <i>Callback</i> and <i>UserData</i>
fields in <a href="RAROpenArchiveDataEx.md">RAROpenArchiveDataEx</a>
structure, when calling <a href="RAROpenArchiveEx.md">RAROpenArchiveEx</a>.
If you use RARSetCallback, you will not be able to read the archive comment
in archives with encrypted headers. If you do not need the archive comment,
you can continue to use RARSetCallback.</p>

<h3>Parameters</h3>

<i>hArcData</i>
<blockquote>
This parameter should contain the archive handle obtained from
<a href="RAROpenArchive.md">RAROpenArchive</a> or
<a href="RAROpenArchiveEx.md">RAROpenArchiveEx</a> function call.
</blockquote>

<i>Callback</i>
<blockquote>
  <p>Address of <a href="RARCallback.md">user defined callback function</a>
  to process UnRAR events.</p>
  <p>Set it to NULL if you do not want to define the callback function.
  Callback function is required to process multivolume and encrypted
  archives properly.</p>
</blockquote>

<i>UserData</i>
<blockquote>
  <p>User defined value, which will be passed to 
  <a href="RARCallback.md">callback function.</a></p>
</blockquote>


<h3>Return values</h3>
<blockquote>
None.
</blockquote>

<h3>See also</h3>
<blockquote>
  <a href="RARCallback.md">User defined callback function</a>
</blockquote>

</body>

</html>
