<!DOCTYPE HTML PUBLIC "-//W3C//DTD HTML 4.01 Transitional//EN">
<html>

<head>
<title>UnRAR.dll Manual</title>
</head>

<body>

<h3>void PASCAL RARSetPassword(HANDLE hArcData,char *Password)</h3>

<h3>Description</h3>

<p>Set a password to decrypt files.</p>

<p>This function does not allow to process archives with encrypted headers.
It can be used only for archives with encrypted file data and unencrypted
headers. So the recommended way to set a password is UCM_NEEDPASSWORDW
message in <a href="RARCallback.md">user defined callback function</a>.
Unlike RARSetPassword, UCM_NEEDPASSWORDW can be used for all types of
encrypted RAR archives.</p>

<h3>Parameters</h3>

<i>hArcData</i>
<blockquote>
This parameter should contain the archive handle obtained from
<a href="RAROpenArchive.md">RAROpenArchive</a> or
<a href="RAROpenArchiveEx.md">RAROpenArchiveEx</a> function call.
</blockquote>

<i>Password</i>
<blockquote>
  Zero terminated string containing a password.
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
