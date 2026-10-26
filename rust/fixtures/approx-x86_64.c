/* RCP/RSQRT: architectural bounds, special values, lanes and unchanged MXCSR. */
static long call(long n,long a,long b,long c) {long r;__asm__ volatile("syscall":"=a"(r):"a"(n),"D"(a),"S"(b),"d"(c):"rcx","r11","memory");return r;}
struct record {unsigned input[4],output[4],previous[4],control,status,op,pair;};
#define OP(instruction) __asm__ volatile("movdqu %2,%%xmm1; movdqu %3,%%xmm0; ldmxcsr %4; " instruction " %%xmm1,%%xmm0; movdqu %%xmm0,%0; stmxcsr %1":"=m"(r.output),"=m"(r.status):"m"(r.input),"m"(r.previous),"m"(r.control):"xmm0","xmm1","memory")
__attribute__((noreturn,force_align_arg_pointer)) void _start(void) {
    static const unsigned edges[]={0,0x80000000,1,0x80000001,0x7fffff,0x807fffff,0x800000,0x80800000,0x3f800000,0xbf800000,0x7f800000,0xff800000,0x7fc00123,0xff800123,0x7f7fffff,0x7e800000};
    for(unsigned op=0;op<4;op++)for(unsigned mode=0;mode<64;mode++)for(unsigned pair=0;pair<512;pair++) {
        struct record r={0};r.op=op;r.pair=pair;
        r.control=((mode&32)?0:0x1f80)|((mode&16)?63:0)|((mode&3)<<13)|((mode&4)?64:0)|((mode&8)?32768:0);
        unsigned long seed=(pair+1)*0x9e3779b97f4a7c15ul;
        for(unsigned lane=0;lane<4;lane++) {
            seed ^= seed>>12;seed ^= seed<<25;seed ^= seed>>27;
            r.input[lane]=pair<16?edges[(pair+lane)%16]:(unsigned)(seed*0x2545f4914f6cdd1dul);
            r.previous[lane]=0x12345678+lane*0x11111111;
        }
        switch(op){case 0:OP("rcpss");break;case 1:OP("rcpps");break;case 2:OP("rsqrtss");break;case 3:OP("rsqrtps");break;}
        if(call(1,1,(long)&r,sizeof r)!=sizeof r){call(231,1,0,0);__builtin_unreachable();}
    }
    call(231,0,0,0);__builtin_unreachable();
}
