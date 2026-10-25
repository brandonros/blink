/* Binary records: compare guest result bits and MXCSR against native SSE2. */
static long call(long n,long a,long b,long c) {
    long r; __asm__ volatile("syscall":"=a"(r):"a"(n),"D"(a),"S"(b),"d"(c):"rcx","r11","memory"); return r;
}
static const unsigned long doubles[]={0,0x8000000000000000ul,0x3ff0000000000000ul,0xbff0000000000000ul,0x3fd5555555555555ul,0x7ff0000000000000ul,0xfff0000000000000ul,0x7ff8000000000123ul,0xfff0000000000123ul,1,0x0010000000000000ul,0x7feffffffffffffful,0x3ca0000000000000ul};
static const unsigned long singles[]={0,0x80000000ul,0x3f800000ul,0xbf800000ul,0x3eaaaaabul,0x7f800000ul,0xff800000ul,0x7fc00123ul,0xff800123ul,1,0x00800000ul,0x7f7ffffful,0x33800000ul};
struct record { unsigned long a,b,result; unsigned control,status,operation,width; };
#define SETUP "movq %2,%%xmm0; movq %3,%%xmm1; ldmxcsr %4; "
#define ARGS :"=r"(r.result),"=m"(r.status):"r"(r.a),"r"(r.b),"m"(r.control):"xmm0","xmm1","memory","cc"
#define OP(instruction) __asm__ volatile(SETUP instruction " %%xmm1,%%xmm0; movq %%xmm0,%0; stmxcsr %1" ARGS)
#define COMPARE(instruction) do { __asm__ volatile(SETUP instruction " %%xmm1,%%xmm0; pushfq; pop %0; stmxcsr %1" ARGS); r.result &= 0x8d5; } while(0)
#define TOINT(instruction) __asm__ volatile(SETUP instruction " %%xmm0,%0; stmxcsr %1" ARGS)
#define TOINT32(instruction) __asm__ volatile(SETUP instruction " %%xmm0,%k0; stmxcsr %1" ARGS)
#define FROMINT32(instruction) __asm__ volatile(SETUP instruction " %k2,%%xmm1; movq %%xmm1,%0; stmxcsr %1" ARGS)
#define CONVERT(instruction) __asm__ volatile(SETUP instruction " %%xmm0,%%xmm1; movq %%xmm1,%0; stmxcsr %1" ARGS)
#define FROMINT(instruction) __asm__ volatile(SETUP instruction " %2,%%xmm1; movq %%xmm1,%0; stmxcsr %1" ARGS)
#define PREDICATE(instruction,predicate) __asm__ volatile(SETUP instruction " $" #predicate ",%%xmm1,%%xmm0; movq %%xmm0,%0; stmxcsr %1" ARGS)
__attribute__((noreturn,force_align_arg_pointer)) void _start(void) {
    for(unsigned width=4;width<=8;width+=4) for(unsigned op=0;op<24;op++)
    for(unsigned mode=0;mode<16;mode++) for(unsigned pair=0;pair<169+256;pair++) {
        const unsigned long *values=width==4?singles:doubles;
        unsigned long left,right;
        if(pair<169) {left=values[pair/13];right=values[pair%13];}
        else {
            unsigned long seed=(pair-168)*0x9e3779b97f4a7c15ul;
            seed ^= seed>>12; seed ^= seed<<25; seed ^= seed>>27; left=seed*0x2545f4914f6cdd1dul;
            seed ^= seed>>12; seed ^= seed<<25; seed ^= seed>>27; right=seed*0x2545f4914f6cdd1dul;
            if(width==4) {left=(unsigned)left;right=(unsigned)right;}
        }
        struct record r={left,right,0,0x1f80|((mode&3)<<13)|((mode&4)?64:0)|((mode&8)?32768:0),0,op,width};
        if(width==8) {
            switch(op) {case 0:OP("addsd");break;case 1:OP("subsd");break;case 2:OP("mulsd");break;case 3:OP("divsd");break;
            case 4:OP("minsd");break;case 5:OP("maxsd");break;
            case 6:COMPARE("comisd");break;case 7:COMPARE("ucomisd");break;
            case 8:FROMINT("cvtsi2sd");break;case 9:TOINT("cvtsd2si");break;case 10:TOINT("cvttsd2si");break;
            case 11:PREDICATE("cmpsd",0);break;case 12:PREDICATE("cmpsd",1);break;case 13:PREDICATE("cmpsd",2);break;case 14:PREDICATE("cmpsd",3);break;
            case 15:PREDICATE("cmpsd",4);break;case 16:PREDICATE("cmpsd",5);break;case 17:PREDICATE("cmpsd",6);break;case 18:PREDICATE("cmpsd",7);break;
            case 19:TOINT32("cvtsd2si");break;case 20:TOINT32("cvttsd2si");break;case 21:FROMINT32("cvtsi2sd");break;case 22:CONVERT("cvtsd2ss");break;case 23:CONVERT("sqrtsd");break;}
        } else {
            switch(op) {case 0:OP("addss");break;case 1:OP("subss");break;case 2:OP("mulss");break;case 3:OP("divss");break;
            case 4:OP("minss");break;case 5:OP("maxss");break;
            case 6:COMPARE("comiss");break;case 7:COMPARE("ucomiss");break;
            case 8:FROMINT("cvtsi2ss");break;case 9:TOINT("cvtss2si");break;case 10:TOINT("cvttss2si");break;
            case 11:PREDICATE("cmpss",0);break;case 12:PREDICATE("cmpss",1);break;case 13:PREDICATE("cmpss",2);break;case 14:PREDICATE("cmpss",3);break;
            case 15:PREDICATE("cmpss",4);break;case 16:PREDICATE("cmpss",5);break;case 17:PREDICATE("cmpss",6);break;case 18:PREDICATE("cmpss",7);break;
            case 19:TOINT32("cvtss2si");break;case 20:TOINT32("cvttss2si");break;case 21:FROMINT32("cvtsi2ss");break;case 22:CONVERT("cvtss2sd");break;case 23:CONVERT("sqrtss");break;}
        }
        if(call(1,1,(long)&r,sizeof r)!=sizeof r) {call(231,1,0,0);__builtin_unreachable();}
    }
    call(231,0,0,0); __builtin_unreachable();
}
