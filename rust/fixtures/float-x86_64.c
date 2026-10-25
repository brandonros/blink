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
#define PACKED(instruction) __asm__ volatile("ldmxcsr %3; movdqu %2,%%xmm0; movdqu %4,%%xmm1; " instruction " %%xmm0,%%xmm1; movdqu %%xmm1,%0; stmxcsr %1" :"=m"(output),"=m"(status):"m"(input),"m"(control),"m"(other):"xmm0","xmm1","memory")
#define FROMMM(instruction) __asm__ volatile("fninit; ldmxcsr %3; movq %2,%%mm0; movdqu %2,%%xmm1; " instruction " %%mm0,%%xmm1; movdqu %%xmm1,%0; stmxcsr %1; emms" :"=m"(output),"=m"(status):"m"(input),"m"(control):"xmm1","mm0","memory")
#define TOMM(instruction) do {output[0]=output[1]=0; __asm__ volatile("fninit; ldmxcsr %3; movdqu %2,%%xmm0; " instruction " %%xmm0,%%mm1; movq %%mm1,%0; stmxcsr %1; emms" :"=m"(output[0]),"=m"(status):"m"(input),"m"(control):"xmm0","mm1","memory");} while(0)
#define PACKCOMPARE(instruction,predicate) __asm__ volatile("ldmxcsr %3; movdqu %2,%%xmm0; movdqu %4,%%xmm1; " instruction " $" #predicate ",%%xmm0,%%xmm1; movdqu %%xmm1,%0; stmxcsr %1" :"=m"(output),"=m"(status):"m"(input),"m"(control),"m"(other):"xmm0","xmm1","memory")
static void packed(void) {
    for(unsigned op=0;op<46;op++) for(unsigned mode=0;mode<16;mode++)
    for(unsigned pair=0;pair<169+256;pair++) {
        unsigned long input[2],output[2],other[2]; unsigned status;
        unsigned control=0x1f80|((mode&3)<<13)|((mode&4)?64:0)|((mode&8)?32768:0);
        if(pair<169) {
            if(op==1||op==2||op==6||op==10||op==11||(op>=14&&op<30&&op%2==0)||(op>=30&&op<38)) {
                input[0]=singles[pair/13]|(singles[pair%13]<<32);
                input[1]=singles[(pair+3)%13]|(singles[(pair+7)%13]<<32);
            } else {input[0]=doubles[pair/13];input[1]=doubles[pair%13];}
        } else {
            unsigned long seed=(pair-168)*0x9e3779b97f4a7c15ul;
            seed ^= seed>>12; seed ^= seed<<25; seed ^= seed>>27; input[0]=seed*0x2545f4914f6cdd1dul;
            seed ^= seed>>12; seed ^= seed<<25; seed ^= seed>>27; input[1]=seed*0x2545f4914f6cdd1dul;
        }
        other[0]=input[1];other[1]=input[0];
        switch(op) {
        case 0:PACKED("cvtdq2ps");break;
        case 1:PACKED("cvtps2dq");break;
        case 2:PACKED("cvttps2dq");break;
        case 3:PACKED("cvtdq2pd");break;
        case 4:PACKED("cvtpd2dq");break;
        case 5:PACKED("cvttpd2dq");break;
        case 6:PACKED("cvtps2pd");break;
        case 7:PACKED("cvtpd2ps");break;
        case 8:FROMMM("cvtpi2ps");break;case 9:FROMMM("cvtpi2pd");break;
        case 10:TOMM("cvtps2pi");break;case 11:TOMM("cvttps2pi");break;
        case 12:TOMM("cvtpd2pi");break;case 13:TOMM("cvttpd2pi");break;
        case 14:PACKED("addps");break;
        case 15:PACKED("addpd");break;
        case 16:PACKED("subps");break;
        case 17:PACKED("subpd");break;
        case 18:PACKED("mulps");break;
        case 19:PACKED("mulpd");break;
        case 20:PACKED("divps");break;
        case 21:PACKED("divpd");break;
        case 22:PACKED("minps");break;
        case 23:PACKED("minpd");break;
        case 24:PACKED("maxps");break;
        case 25:PACKED("maxpd");break;
        case 26:PACKED("sqrtps");break;
        case 27:PACKED("sqrtpd");break;
        case 28:PACKED("andps");break;
        case 29:PACKED("andpd");break;
        case 30:PACKCOMPARE("cmpps",0);break;
        case 31:PACKCOMPARE("cmpps",1);break;
        case 32:PACKCOMPARE("cmpps",2);break;
        case 33:PACKCOMPARE("cmpps",3);break;
        case 34:PACKCOMPARE("cmpps",4);break;
        case 35:PACKCOMPARE("cmpps",5);break;
        case 36:PACKCOMPARE("cmpps",6);break;
        case 37:PACKCOMPARE("cmpps",7);break;
        case 38:PACKCOMPARE("cmppd",0);break;
        case 39:PACKCOMPARE("cmppd",1);break;
        case 40:PACKCOMPARE("cmppd",2);break;
        case 41:PACKCOMPARE("cmppd",3);break;
        case 42:PACKCOMPARE("cmppd",4);break;
        case 43:PACKCOMPARE("cmppd",5);break;
        case 44:PACKCOMPARE("cmppd",6);break;
        case 45:PACKCOMPARE("cmppd",7);break;

        }
        for(unsigned half=0;half<2;half++) {
            struct record r={input[0],input[1],output[half],control,status,24+op,16+half};
            if(call(1,1,(long)&r,sizeof r)!=sizeof r) {call(231,1,0,0);__builtin_unreachable();}
        }
    }
}
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
    packed();
    call(231,0,0,0); __builtin_unreachable();
}
