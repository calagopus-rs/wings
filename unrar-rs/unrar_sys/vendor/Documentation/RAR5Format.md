<!DOCTYPE HTML PUBLIC "-//W3C//DTD HTML 4.01 Transitional//EN">
<html>
<head>
<TITLE>RAR 5.0 archive format</TITLE>
</head>

<body>

<P class="title"><A NAME="HELPRAR5Format"></A>RAR 5.0 archive format<hr></P>


<P>WinRAR 5.0 introduces a new verson of RAR archive format.</P>

<P>New features of RAR 5.0 format include:</P>

<ul>
<li>Compression <a href="HELPGetArcGeneral.htm#DictSize">dictionary size</a>
up to 1 GB. When compressing big files, especially
in <a href="HELPArcSolid.htm">solid</a> mode, larger dictionary frequently
allows to achieve a higher compression ratio.</li>

<li>Encryption based on AES-256 algorithm, which is theoretically
stronger than RAR 4.x AES-128.</li>

<li><a href="HELPArcRecovery.htm">Recovery record</a> using Reed-Solomon
error correction codes with much higher resistance to multiple damages
comparing to RAR 4.x recovery record.</li>

<li>Faster <a href="HELPArcRecVolumes.htm">recovery volume</a> operations.
Maximum number of RAR+REV volumes in RAR 5.0 is 65535 instead of 255.</li>

<li>File times stored as Coordinated Universal Time (UTC) instead of
RAR 4.x local time, making file exchange among several time zones
more straightforward.</li>

<li>Optional <a href="HELPGetArcOptions.htm#BLAKE2">BLAKE2sp checksums</a>
for file data. Unlike CRC32 checksum, it is practically impossible 
for BLAKE2 to have two different files with the same checksum value.
So BLAKE2 can be used for file identification purpose.</li>

<li>Optional <a href="HELPGetArcOptions.htm#QO">quick open information</a>
can be added to archive to provide a faster access to archive contents.</li>

<li>Multithreading support in decompression algorithm. Its speed benefit
is more noticeable on large files with poorly compressible data or with
BLAKE2 checksums.</li>

<li>Support for NTFS 
<a href="HELPGetArcAdvanced.htm#symlinks">reparse points</a>,
<a href="HELPGetArcAdvanced.htm#symlinks">symbolic links</a>,
<a href="HELPGetArcAdvanced.htm#hardlinks">hard links</a>.</li>

<li>Possibility to store second and following copies of 
<a href="HELPGetArcOptions.htm#ident">identical files</a>
as references to first copy of such file.</li>

<li>Complete Unicode awareness. File names and comments are stored in UTF-8
format.</li>
</ul>

<br>
<p>Some obsolete or inefficient RAR 4.x features are not included in
RAR 5.0 format:</p>

<ul>
<li>Special algorithms including text compression, raw audio and true color
compression, cannot be used in RAR 5.0 archives. Those formats of raw audio
and true color data, which were recognized by older RAR versions,
become significantly less widespread. Text compression performance
on modern multi-core CPUs is too low comparing to general compression.
Even though these algorithms are not supported in RAR 5.0, latest WinRAR
can decompress any 4.x RAR archives, including those using mentioned above
algorithms. RAR 5.0 still includes Intel IA-32 executable and delta
compression algorithms, which are efficient for modern data and hardware.</li>

<li>Creating old style volume names in archive.r00, archive.r01 format
is not supported. Main purpose of these names was compatibility with
MS DOS file system. But such naming convention does not work well with
platforms like Windows, where file extensions are used to define file types
and associations.</li>
</ul>

<br>
<p>Older applications including WinRAR versions earlier than 5.0 are not
able to recognize RAR 5.0 archives. So if you wish to be sure that archive
can be decompressed by older software, you may prefer to select RAR 4.x
archiving format. It can be done either in 
<a href="HELPGetArcGeneral.htm#SetFmt">Archive name and parameters</a> dialog
or with <a href="HELPSwMA.htm">-ma command line switch</a>.
Also RAR 4.x may be preferable if you need special text or multimedia
compression algorithms or old style volume naming scheme.
</p>

</body>
</html>
