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
    for (u64 i=0;i<257;i++) {
        x = ((x << 7) | (x >> 57)) ^ (y+i);
        y = y*inputs[3]+x;
    }
    char hex[33];
    for(int i=0;i<16;i++) {unsigned n=(x>>(i*4))&15;hex[i]="0123456789abcdef"[n];}
    for(int i=0;i<16;i++) {unsigned n=(y>>(i*4))&15;hex[i+16]="0123456789abcdef"[n];}
    hex[32]='\n';
    call(1,1,(long)hex,33);call(231,fail,0,0);
    __builtin_unreachable();
}
