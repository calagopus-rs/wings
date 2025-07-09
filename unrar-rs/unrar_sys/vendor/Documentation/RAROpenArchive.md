<!DOCTYPE HTML PUBLIC "-//W3C//DTD HTML 4.01 Transitional//EN">
<html>

<head>
<title>UnRAR.dll Manual</title>
</head>

<body>

<h3>HANDLE PASCAL RAROpenArchive(struct RAROpenArchiveData *ArchiveData)</h3>

<h3>Description</h3>
<p>Open RAR archive and allocate memory structures.</p>
<p>This function is obsolete. It does not support Unicode names
and does not allow to specify the callback function. It is recommended
to use <a href="RAROpenArchiveEx.md">RAROpenArchiveEx</a> instead.<p>

<h3>Parameters</h3>

<i>ArchiveData</i>
<blockquote>
Points to <a href="RAROpenArchiveData.md">RAROpenArchiveData structure</a>.
</blockquote>

<h3>Return values</h3>
<blockquote>
Archive handle or NULL in case of error.
</blockquote>

<h3>See also</h3>
<blockquote>
  <a href="RAROpenArchiveData.md">RAROpenArchiveData</a> structure.
</blockquote>


</body>

</html>
