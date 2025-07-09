<!DOCTYPE HTML PUBLIC "-//W3C//DTD HTML 4.01 Transitional//EN">
<html>

<head>
<title>UnRAR.dll Manual</title>
</head>

<body>

<h3>int PASCAL RARGetDllVersion()</h3>

<h3>Description</h3>

Returns API version.

<h3>Parameters</h3>

None.

<h3>Return values</h3>
  <p>Returns an integer value denoting UnRAR.dll API version, which is also
defined in unrar.h as RAR_DLL_VERSION. API version number is incremented
only in case of noticeable changes in UnRAR.dll API. Do not confuse it
with version of UnRAR.dll stored in DLL resources, which is incremented
with every DLL rebuild.</p>

  <p>If RARGetDllVersion() returns a value lower than UnRAR.dll, which your
application was designed for, it may indicate that DLL version is too old
and it may fail to provide all necessary functions to your application.</p>

  <p>This function is missing in very old versions of UnRAR.dll,
so it is safer to use LoadLibrary and GetProcAddress to access it.</p>

</body>

</html>
