"use strict";

// The released v0.1.1 Windows server serialises a Windows path directly
// after file://, escaping its backslashes. Match that wire format so imports
// and open documents have the same identity inside that server.
/** @param {string} file @returns {string | undefined} */
function windowsServerUri(file) {
  if (!/^[A-Za-z]:[\\/]/.test(file) && !file.startsWith("\\\\")) return;
  file = file.replace(/^([A-Z]):/, (drive) => drive.toLowerCase());
  let result = "file://";
  for (const byte of Buffer.from(file, "utf8")) {
    const character = String.fromCharCode(byte);
    result += /[A-Za-z0-9\-_.~/:]/.test(character) ? character : `%${byte.toString(16).toUpperCase().padStart(2, "0")}`;
  }
  return result;
}

/** @param {string} uri @returns {string | undefined} */
function windowsServerPath(uri) {
  if (!uri.startsWith("file://")) return;
  try {
    const file = decodeURIComponent(uri.slice(7));
    if (/^[A-Za-z]:[\\/]/.test(file) || file.startsWith("\\\\")) return file;
  } catch { /* Leave malformed or other-platform URIs to the standard parser. */ }
}
module.exports = { windowsServerUri, windowsServerPath };
