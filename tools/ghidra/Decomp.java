// Decompiles every function whose name contains one of the given substrings.
import ghidra.app.script.GhidraScript;
import ghidra.app.decompiler.*;
import ghidra.program.model.listing.*;
public class Decomp extends GhidraScript {
  @Override public void run() throws Exception {
    String[] wanted = getScriptArgs();
    DecompInterface d = new DecompInterface(); d.openProgram(currentProgram);
    FunctionIterator it = currentProgram.getFunctionManager().getFunctions(true);
    while (it.hasNext()) {
      Function f = it.next(); String n = f.getName(true);
      for (String w : wanted) if (n.contains(w)) {
        DecompileResults r = d.decompileFunction(f, 120, monitor);
        println("==== " + n + " @ " + f.getEntryPoint());
        println(r.decompileCompleted() ? r.getDecompiledFunction().getC() : "(failed)");
      }
    }
  }
}
