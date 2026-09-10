export function initialToolDetailsOpen(
  mode: "auto" | "collapsed" | "expanded",
  status: string,
): boolean {
  if (mode === "collapsed") return false;
  if (mode === "expanded") return true;
  return status !== "done";
}
