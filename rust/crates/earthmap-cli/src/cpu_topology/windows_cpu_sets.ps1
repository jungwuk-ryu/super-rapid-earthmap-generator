# Read CPU-set metadata through managed interop, without unsafe Rust or affinity changes.
# Higher EfficiencyClass values mean faster cores. Keep processor-group IDs in core keys.
$ErrorActionPreference = 'Stop'
Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;

public static class EarthmapCpuTopologyProbe {
    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern IntPtr OpenProcess(uint access, bool inherit, uint processId);
    [DllImport("kernel32.dll")]
    private static extern bool CloseHandle(IntPtr handle);
    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool GetSystemCpuSetInformation(IntPtr buffer, uint size,
        out uint returnedSize, IntPtr process, uint flags);
    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool GetProcessDefaultCpuSets(IntPtr process, [Out] uint[] ids,
        uint count, out uint requiredCount);
    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool GetProcessAffinityMask(IntPtr process,
        out UIntPtr processMask, out UIntPtr systemMask);
    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool GetProcessGroupAffinity(IntPtr process,
        ref ushort count, [Out] ushort[] groups);

    public static string Read(uint processId) {
        IntPtr process = OpenProcess(0x1000, false, processId);
        if (process == IntPtr.Zero) throw new InvalidOperationException("process query failed");
        try {
            uint requiredIds;
            GetProcessDefaultCpuSets(process, null, 0, out requiredIds);
            if (requiredIds > 65536) throw new InvalidOperationException("CPU-set count is invalid");
            HashSet<uint> selected = new HashSet<uint>();
            if (requiredIds > 0) {
                uint[] ids = new uint[requiredIds];
                uint actual;
                if (!GetProcessDefaultCpuSets(process, ids, requiredIds, out actual) || actual > requiredIds)
                    throw new InvalidOperationException("CPU-set selection changed");
                for (int i = 0; i < actual; i++) selected.Add(ids[i]);
            }
            ushort groupCount = 0;
            GetProcessGroupAffinity(process, ref groupCount, null);
            ushort[] groups = new ushort[groupCount];
            bool haveGroups = groupCount > 0 && GetProcessGroupAffinity(process, ref groupCount, groups);
            UIntPtr processMask, systemMask;
            bool haveMask = GetProcessAffinityMask(process, out processMask, out systemMask);
            // A multi-group process has no single affinity mask. Its CPU-set
            // selection and Rust's available_parallelism still bound the pool.
            bool partialMask = haveGroups && groupCount == 1 && haveMask
                && processMask != UIntPtr.Zero && processMask != systemMask;

            uint required;
            GetSystemCpuSetInformation(IntPtr.Zero, 0, out required, process, 0);
            for (int attempt = 0; attempt < 3; attempt++) {
                if (required == 0 || required > 1024 * 1024)
                    throw new InvalidOperationException("CPU-set buffer size is invalid");
                IntPtr buffer = Marshal.AllocHGlobal((int)required);
                try {
                    uint actual;
                    if (!GetSystemCpuSetInformation(buffer, required, out actual, process, 0)) {
                        if (actual > required) { required = actual; continue; }
                        throw new InvalidOperationException("CPU-set query failed");
                    }
                    if (actual > required) throw new InvalidOperationException("CPU-set buffer changed");
                    StringBuilder result = new StringBuilder();
                    for (int offset = 0; offset < actual;) {
                        if (actual - offset < 8) throw new InvalidOperationException("CPU-set header is truncated");
                        IntPtr record = IntPtr.Add(buffer, offset);
                        int size = Marshal.ReadInt32(record, 0);
                        int type = Marshal.ReadInt32(record, 4);
                        if (size < 8 || size > actual - offset)
                            throw new InvalidOperationException("CPU-set record is invalid");
                        if (type == 0) {
                            if (size < 24) throw new InvalidOperationException("CPU-set record is truncated");
                            uint id = unchecked((uint)Marshal.ReadInt32(record, 8));
                            ushort group = unchecked((ushort)Marshal.ReadInt16(record, 12));
                            byte logical = Marshal.ReadByte(record, 14);
                            byte core = Marshal.ReadByte(record, 15);
                            byte efficiency = Marshal.ReadByte(record, 18);
                            byte flags = Marshal.ReadByte(record, 19);
                            bool allocatedElsewhere = (flags & 2) != 0 && (flags & 4) == 0;
                            bool allowed = !allocatedElsewhere && (selected.Count == 0 || selected.Contains(id));
                            if (partialMask) {
                                allowed = allowed && group == groups[0] && logical < 64
                                    && (processMask.ToUInt64() & (1UL << logical)) != 0;
                            }
                            if (allowed) {
                                result.Append(group).Append(',').Append(logical).Append(',')
                                    .Append(core).Append(',').Append(efficiency).AppendLine();
                            }
                        }
                        offset += size;
                    }
                    return result.ToString();
                } finally { Marshal.FreeHGlobal(buffer); }
            }
            throw new InvalidOperationException("CPU-set topology kept changing");
        } finally { CloseHandle(process); }
    }
}
'@
if (-not $EarthmapCpuCompileOnly) {
    [EarthmapCpuTopologyProbe]::Read([uint32]$EarthmapCpuProcessId)
}
