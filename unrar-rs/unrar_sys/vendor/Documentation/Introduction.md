<!DOCTYPE HTML PUBLIC "-//W3C//DTD HTML 4.01 Transitional//EN">
<html>

<head>
<title>UnRAR.dll Manual</title>
</head>

<body>

<p>UnRAR.dll is Windows dynamic-link library, which provides file extraction.
from RAR archives. It is available both as 32-bit (unrar.dll) and 64-bit x64
(unrar64.dll) versions.</p>

<p>You can find samples of UnRAR.dll use in "Examples" folder.
All samples except C sample (UnRDLL.c) are contributed by unrar.dll users.
We at rarlab.com created and tested only UnRDLL.c sample.</p>

<p>Brief scenario of unrar.dll use includes the call of 
<a href="RAROpenArchiveEx.md">RAROpenArchiveEx</a> to open an archive,
<a href="RARReadHeaderEx.md">RARReadHeaderEx</a> to read archive headers,
<a href="RARProcessFileW.md">RARProcessFileW</a> to process read header
and <a href="RARCloseArchive.md">RARCloseArchive</a> to close the archive
and free all previously allocated objects.<p>

<p>If you use this library in Unix, you may need to call
setlocale(LC_CTYPE, "") in your application, so Unicode conversion functions
work properly.

<p>Please read <a href="index.md#functions">the functions description</a>
and look into source code in "Examples" folder for more details.</p>


</body>

</html>
