// Lists every function whose name contains one of the given substrings, with entry and size.
import ghidra.app.script.GhidraScript;
import ghidra.program.model.listing.*;
public class ListFuncs extends GhidraScript {
  @Override public void run() throws Exception {
    String[] wanted = getScriptArgs();
    FunctionIterator it = currentProgram.getFunctionManager().getFunctions(true);
    while (it.hasNext()) {
      Function f = it.next(); String n = f.getName(true);
      for (String w : wanted) if (n.contains(w)) {
        println("FN " + f.getEntryPoint() + " " + f.getBody().getNumAddresses() + " " + n);
        break;
      }
    }
  }
}
