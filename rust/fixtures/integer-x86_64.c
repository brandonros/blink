/* Freestanding compiled-C guest: no libc, no host-side computation fallback. */
typedef unsigned long u64;
static volatile u64 inputs[] = {0x123456789abcdef0ul, 0xfedcba9876543210ul, 17, 31};
static long call(long n, long a, long b, long c) {
    long r;
    __asm__ volatile("syscall" : "=a"(r) : "a"(n), "D"(a), "S"(b), "d"(c) : "rcx", "r11", "memory");
    return r;
}
__attribute__((noreturn)) void _start(void) {
    u64 x=inputs[0], y=inputs[1], divisor=inputs[2];
    u64 quotient=y/divisor, remainder=y%divisor;
    int fail=quotient*divisor+remainder!=y;
    fail |= __builtin_bswap32((unsigned)inputs[0]) != 0xf0debc9au;
    for (u64 i=0;i<257;i++) {
        x = ((x << 7) | (x >> 57)) ^ (y+i);
        y = y*inputs[3]+x;
    }
    /* Results and architecturally defined CF/ZF, including zero and all bit positions. */
    for(int bit=-1;bit<64;bit++) {
        u64 source=bit<0?0:1ul<<bit, result, flags;
        __asm__ volatile("tzcntq %2,%0; pushfq; popq %1":"=&r"(result),"=r"(flags):"r"(source):"cc");
        unsigned expected=bit<0?64:(unsigned)bit;
        fail |= result!=expected || (flags&65)!=((bit<0?1:0)|(expected==0?64:0));
        __asm__ volatile("lzcntq %2,%0; pushfq; popq %1":"=&r"(result),"=r"(flags):"r"(source):"cc");
        expected=bit<0?64:63-(unsigned)bit;
        fail |= result!=expected || (flags&65)!=((bit<0?1:0)|(expected==0?64:0));
        __asm__ volatile("tzcntl %k2,%k0; pushfq; popq %1":"=&r"(result),"=r"(flags):"r"(source):"cc");
        expected=bit<0||bit>=32?32:(unsigned)bit;
        fail |= result!=expected || (flags&65)!=((expected==32?1:0)|(expected==0?64:0));
        __asm__ volatile("lzcntl %k2,%k0; pushfq; popq %1":"=&r"(result),"=r"(flags):"r"(source):"cc");
        expected=bit<0||bit>=32?32:31-(unsigned)bit;
        fail |= result!=expected || (flags&65)!=((expected==32?1:0)|(expected==0?64:0));
        result=0x123456789abcdef0;
        __asm__ volatile("tzcntw %w2,%w0; pushfq; popq %1":"+r"(result),"=r"(flags):"r"(source):"cc");
        expected=bit<0||bit>=16?16:(unsigned)bit;
        fail |= result!=(0x123456789abc0000ul|expected) || (flags&65)!=((expected==16?1:0)|(expected==0?64:0));
        __asm__ volatile("lzcntw %w2,%w0; pushfq; popq %1":"+r"(result),"=r"(flags):"r"(source):"cc");
        expected=bit<0||bit>=16?16:15-(unsigned)bit;
        fail |= result!=(0x123456789abc0000ul|expected) || (flags&65)!=((expected==16?1:0)|(expected==0?64:0));
    }
    u64 streamed=0;
    __asm__ volatile("movnti %1,%0; sfence":"=m"(streamed):"r"(x):"memory");
    fail |= streamed!=x;
    char hex[33];
    for(int i=0;i<16;i++) {unsigned n=(x>>(i*4))&15;hex[i]="0123456789abcdef"[n];}
    for(int i=0;i<16;i++) {unsigned n=(y>>(i*4))&15;hex[i+16]="0123456789abcdef"[n];}
    hex[32]='\n';
    call(1,1,(long)hex,33);call(231,fail,0,0);
    __builtin_unreachable();
}
