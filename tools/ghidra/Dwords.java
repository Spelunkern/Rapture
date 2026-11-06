// Prints N little-endian dwords at an address: Dwords.java <address> <count>.
import ghidra.app.script.GhidraScript;
public class Dwords extends GhidraScript {
  @Override public void run() throws Exception {
    String[] a = getScriptArgs();
    var addr = toAddr(a[0]);
    int n = Integer.parseInt(a[1]);
    StringBuilder sb = new StringBuilder();
    for (int i = 0; i < n; i++) sb.append(currentProgram.getMemory().getInt(addr.add(i * 4))).append(' ');
    println("DW " + a[0] + ": " + sb);
  }
}
