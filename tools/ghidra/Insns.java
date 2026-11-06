// Prints the instructions of functions at the given addresses whose text contains the filter (last argument).
import ghidra.app.script.GhidraScript;
import ghidra.program.model.listing.*;
public class Insns extends GhidraScript {
  @Override public void run() throws Exception {
    String[] a = getScriptArgs();
    String filter = a[a.length - 1];
    for (int i = 0; i < a.length - 1; i++) {
      Function f = getFunctionAt(toAddr(a[i]));
      println("==== " + f.getName(true) + " @ " + f.getEntryPoint());
      for (Instruction ins : currentProgram.getListing().getInstructions(f.getBody(), true)) {
        String t = ins.toString();
        if (t.contains(filter)) println("  " + ins.getAddress() + " " + t);
      }
    }
  }
}
