// Stands in for a page's own external script module. If the harness did not
// follow the `src`, this body would never run and `fromExternalFile` would be
// undefined rather than answering from here.
function fromExternalFile() {
  return "loaded from its own file";
}