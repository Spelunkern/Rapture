// Prints every reference to the given addresses with the referencing function.
import ghidra.app.script.GhidraScript;
import ghidra.program.model.address.Address;
import ghidra.program.model.listing.Function;
import ghidra.program.model.symbol.Reference;
public class Refs extends GhidraScript {
  @Override public void run() throws Exception {
    for (String a : getScriptArgs()) {
      Address to = toAddr(a);
      for (Reference r : getReferencesTo(to)) {
        Function f = getFunctionContaining(r.getFromAddress());
        println("REF " + a + " <- " + r.getFromAddress() + " " + (f == null ? "?" : f.getName(true) + " @ " + f.getEntryPoint()));
      }
    }
  }
}
