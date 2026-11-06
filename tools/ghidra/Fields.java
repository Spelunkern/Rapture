// Prints offset, size and name of every field of the named structures.
import ghidra.app.script.GhidraScript;
import ghidra.program.model.data.*;
import java.util.Iterator;
public class Fields extends GhidraScript {
  @Override public void run() throws Exception {
    for (String name : getScriptArgs()) {
      Iterator<DataType> it = currentProgram.getDataTypeManager().getAllDataTypes();
      while (it.hasNext()) {
        DataType d = it.next();
        if (!(d instanceof Structure) || !d.getName().equals(name)) continue;
        println("==== " + d.getPathName() + " size=" + d.getLength());
        for (DataTypeComponent c : ((Structure) d).getComponents())
          if (c.getFieldName() != null) println(String.format("  +0x%x %d %s %s", c.getOffset(), c.getLength(), c.getDataType().getName(), c.getFieldName()));
      }
    }
  }
}
