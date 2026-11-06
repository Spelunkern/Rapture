// For each symbol name argument: address, block, 81 floats if initialized, and referencing functions.
import ghidra.app.script.GhidraScript;
import ghidra.program.model.symbol.*;
import ghidra.program.model.address.*;
import ghidra.program.model.listing.*;
public class Sym extends GhidraScript {
  @Override public void run() throws Exception {
    for (String name : getScriptArgs()) {
      for (Symbol s : currentProgram.getSymbolTable().getAllSymbols(true)) {
        if (!s.getName(true).contains(name)) continue;
        Address a = s.getAddress();
        println("==== " + s.getName(true) + " @ " + a + " block=" + currentProgram.getMemory().getBlock(a));
        StringBuilder sb = new StringBuilder();
        try { for (int i = 0; i < 81; i++) sb.append(Float.intBitsToFloat(currentProgram.getMemory().getInt(a.add(i*4)))).append(i%9==8?" | ":" "); } catch (Exception e) { sb.append("(uninitialized)"); }
        println(sb.toString());
        for (Reference r : getReferencesTo(a)) {
          Function f = getFunctionContaining(r.getFromAddress());
          println("  ref from " + r.getFromAddress() + " " + (f == null ? "?" : f.getName(true)) + " " + r.getReferenceType());
        }
      }
    }
  }
}
