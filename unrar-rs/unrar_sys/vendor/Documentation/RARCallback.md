<!DOCTYPE HTML PUBLIC "-//W3C//DTD HTML 4.01 Transitional//EN">
<html>

<head>
<title>UnRAR.dll Manual</title>
</head>

<body>

<h3>User defined callback function</h3>

int CALLBACK CallbackProc(UINT msg,LPARAM UserData,LPARAM P1,LPARAM P2);

<h3>Description</h3>
<p>This function is called by UnRAR.dll library to process different
UnRAR events listed below. You can specify the address of this function
either in <a href="RAROpenArchiveEx.md">RAROpenArchiveEx</a> (preferable)
or in <a href="RARSetCallback.md">RARSetCallback</a> (obsolete)</p>

<h3>Function parameters</h3>

<p>The function will be passed four parameters:</p>

<ul>
<li>
<a name="msg"></a>
<i>msg</i>
<blockquote>
  <p>Event type. Possible values:</p>

  <ul>
  <li>
  <b>UCM_CHANGEVOLUME<br>
     UCM_CHANGEVOLUMEW</b>
    <blockquote>
       <p>Process volume change.</p>
       
       <p><b>P1</b> points to the zero terminated name of the next volume.</p>

       <p><b>P2</b> defines the function call mode.</p>
         <table border=1><tr>
         <td>RAR_VOL_ASK</td>
         <td>Required volume is absent. The function should prompt user
             and return 1 to retry or return -1 value to terminate operation.
             The function may also set a new volume name, placing up to
             1024 characters to the address specified by P1 parameter.<br><br>
             Volume name uses single byte ANSI encoding for UCM_CHANGEVOLUME
             and Unicode for UCM_CHANGEVOLUMEW. If application provides
             a new volume name in UCM_CHANGEVOLUMEW, UCM_CHANGEVOLUME
             is not sent.
             </td>
         </tr><tr>
         <td>RAR_VOL_NOTIFY</td>
         <td>Required volume is successfully opened. This is a notification
             call and volume name modification is not allowed. The function
             should return 1 to continue or -1 to terminate operation.
             Volume name uses single byte ANSI encoding for UCM_CHANGEVOLUME
             and Unicode for UCM_CHANGEVOLUMEW.
             </td>
         </tr></table>
    </blockquote>

  <li>
  <b>UCM_PROCESSDATA</b>
    <blockquote>
       <p>Process unpacked data. It can be used to read a file from memory,
          while it is being extracted or tested. If you use this event
          while testing a file, then it makes possible to read file data
          without extracting file to disk.</p>

       <p>Return 1 to continue processing or -1 to cancel the archive
          operation.</p>

       <p><b>P1</b> contains the address pointing to the unpacked data.
          Callback function can read the data but must not modify it.</p>

       <p><b>P2</b> contains the size of the unpacked data. It is guaranteed
          that this size will not exceed the maximum dictionary size
          (4 MB in RAR 3.0).</p>
    </blockquote>
  <li>
  <b>UCM_NEEDPASSWORD<br>
     UCM_NEEDPASSWORDW</b>
    <blockquote>
       <p>DLL needs a password to process an archive. This message must be
          processed if you wish to be able to handle encrypted archives.</p>

       <p>Return 1 to continue process or -1 to cancel the archive
          operation.</p>

       <p><b>P1</b> contains the address pointing to the buffer for
          a password. You need to copy a password here. Password uses
          the single byte ANSI encoding for UCM_NEEDPASSWORD
          and little endian Unicode for UCM_NEEDPASSWORDW. If application
          provides a password in UCM_NEEDPASSWORDW, UCM_NEEDPASSWORD
          is not sent.</p>

       <p><b>P2</b> contains the size of password buffer in characters.</p>
    </blockquote>
  </ul>
</blockquote>

<li>
<i>UserData</i>
<blockquote>
  <p>User defined value passed to RARSetCallback. You can specify it,
  when defining the callback function in 
  <a href="RAROpenArchiveEx.md">RAROpenArchiveEx</a>
  or <a href="RARSetCallback.md">RARSetCallback</a></p>
</blockquote>


<li>
<i>P1</i><br>
<i>P2</i>
<blockquote>
  <p>Event dependent parameters. Read <a href="#msg"><i>msg</i> description</a>
  above for detailed information.<p>
</blockquote>
</ul>

<h3>Return values</h3>
<p>In general returning -1 means canceling the operation and returning
1 continues processing. Please also read event descriptions above 
in case we'll add some exceptions from this rule.
Return 0 for those event types which you do not process including
unknown event types.
</p>

<h3>Notes</h3>
<blockquote>
  Other UnRAR.dll functions must not be called from the callback function.
</blockquote>

</body>

</html>
