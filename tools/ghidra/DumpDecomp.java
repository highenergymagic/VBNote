// Dump decompiled C for every function plus a map of imported-symbol
// references, so the interesting routines can be found without reading all of
// it. Java rather than Python because Ghidra 12 headless no longer ships
// Jython.
// @category VBNote

import java.io.FileWriter;
import java.io.PrintWriter;

import ghidra.app.decompiler.DecompInterface;
import ghidra.app.decompiler.DecompileResults;
import ghidra.app.script.GhidraScript;
import ghidra.program.model.listing.Function;
import ghidra.program.model.listing.FunctionManager;
import ghidra.program.model.symbol.Reference;
import ghidra.program.model.symbol.Symbol;
import ghidra.program.model.symbol.SymbolIterator;
import ghidra.program.model.symbol.SymbolTable;
import ghidra.util.task.ConsoleTaskMonitor;

public class DumpDecomp extends GhidraScript {
    @Override
    public void run() throws Exception {
        String out = System.getenv("VBNOTE_DECOMP_OUT");
        if (out == null || out.isEmpty()) {
            out = "decomp.c";
        }
        PrintWriter w = new PrintWriter(new FileWriter(out));

        DecompInterface iface = new DecompInterface();
        iface.openProgram(currentProgram);
        FunctionManager fm = currentProgram.getFunctionManager();
        ConsoleTaskMonitor monitor = new ConsoleTaskMonitor();

        w.println("// " + currentProgram.getName());
        w.println();
        w.println("// ---- references to imported symbols ----");
        SymbolTable st = currentProgram.getSymbolTable();
        SymbolIterator it = st.getExternalSymbols();
        while (it.hasNext()) {
            Symbol s = it.next();
            Reference[] refs = s.getReferences();
            if (refs.length == 0) {
                continue;
            }
            w.println("// " + s.getName());
            for (Reference r : refs) {
                Function f = fm.getFunctionContaining(r.getFromAddress());
                w.println("//     from " + r.getFromAddress() + " in "
                        + (f == null ? "?" : f.getName()));
            }
        }
        w.println();

        int count = 0;
        for (Function f : fm.getFunctions(true)) {
            DecompileResults res = iface.decompileFunction(f, 60, monitor);
            w.println("// ======== " + f.getName() + " @ " + f.getEntryPoint() + " ========");
            if (res != null && res.decompileCompleted()) {
                w.println(res.getDecompiledFunction().getC());
            } else {
                w.println("// decompilation failed");
            }
            w.println();
            count++;
        }
        w.close();
        println("DumpDecomp: wrote " + count + " functions to " + out);
    }
}
