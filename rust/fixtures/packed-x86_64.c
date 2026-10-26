/* Compare packed SSE2 lanes and preserved integer flags against native AMD64. */
static long call(long n,long a,long b,long c) {long r;__asm__ volatile("syscall":"=a"(r):"a"(n),"D"(a),"S"(b),"d"(c):"rcx","r11","memory");return r;}
struct record {unsigned long a[2],b[2],result[2],flags;unsigned op,variant;};
#define SETUP "movdqu %2,%%xmm0; movdqu %3,%%xmm1; pushq $0x8d7; popfq; "
#define FINISH "; movdqu %%xmm0,%0; pushfq; popq %1"
#define ARGS :"=m"(r.result),"=r"(r.flags):"m"(r.a),"m"(r.b):"xmm0","xmm1","cc","memory"
#define OP(instruction) __asm__ volatile(SETUP instruction " %%xmm1,%%xmm0" FINISH ARGS)
#define IMM(instruction,value) __asm__ volatile(SETUP instruction " $" #value ",%%xmm1,%%xmm0" FINISH ARGS)
#define SHUFFLE(instruction) switch(variant) {case 0:IMM(instruction,0);break;case 1:IMM(instruction,27);break;case 2:IMM(instruction,136);break;case 3:IMM(instruction,228);break;case 4:IMM(instruction,255);break;case 5:IMM(instruction,57);break;case 6:IMM(instruction,78);break;case 7:IMM(instruction,177);break;}
#define insert(value) __asm__ volatile(SETUP "movd %%xmm1,%%eax; pinsrw $" #value ",%%eax,%%xmm0" FINISH ARGS,"rax")
#define extract(value) __asm__ volatile(SETUP "pextrw $" #value ",%%xmm1,%%eax; movd %%eax,%%xmm0" FINISH ARGS,"rax")
#define SELECT(operation) switch(variant) {case 0:operation(0);break;case 1:operation(1);break;case 2:operation(6);break;case 3:operation(7);break;case 4:operation(8);break;case 5:operation(15);break;case 6:operation(128);break;case 7:operation(255);break;}

#define MMSETUP "fninit; movq %2,%%mm0; movq %3,%%mm1; pushq $0x8d7; popfq; "
#define MMFINISH "; movq %%mm0,%0; emms; pushfq; popq %1"
#define MMARGS :"=m"(r.result),"=r"(r.flags):"m"(r.a),"m"(r.b):"mm0","mm1","cc","memory"
#define MMOP(instruction) __asm__ volatile(MMSETUP instruction " %%mm1,%%mm0" MMFINISH MMARGS)
#define mminsert(value) __asm__ volatile(MMSETUP "movd %%mm1,%%eax; pinsrw $" #value ",%%eax,%%mm0" MMFINISH MMARGS,"rax")
#define mmextract(value) __asm__ volatile(MMSETUP "pextrw $" #value ",%%mm1,%%eax; movd %%eax,%%mm0" MMFINISH MMARGS,"rax")
#define mmshuffle(value) __asm__ volatile(MMSETUP "pshufw $" #value ",%%mm1,%%mm0" MMFINISH MMARGS)
__attribute__((noreturn,force_align_arg_pointer)) void _start(void) {
    const unsigned long counts[]={0,1,15,16,31,32,63,64,255,0x100000000ul};
    const unsigned long edges[]={0,~0ul,0x80007fff0080ff80ul,0x800000007ffffffful,0x00ff0100ffff0000ul,0x7f807f807f807f80ul};
    for(unsigned op=0;op<48;op++) for(unsigned variant=0;variant<8;variant++) for(unsigned pair=0;pair<64;pair++) {
        unsigned long seed=(pair+1)*0x9e3779b97f4a7c15ul;
        struct record r={0};r.op=op;r.variant=variant;
        for(unsigned n=0;n<2;n++) {
            seed ^= seed>>12;seed ^= seed<<25;seed ^= seed>>27;r.a[n]=pair<6?edges[(pair+n)%6]:seed*0x2545f4914f6cdd1dul;
            seed ^= seed>>12;seed ^= seed<<25;seed ^= seed>>27;r.b[n]=pair<6?edges[(pair+3+n)%6]:seed*0x2545f4914f6cdd1dul;
        }
        if(op==23 && pair==0) {r.a[0]=r.a[1]=r.b[0]=r.b[1]=0x8000800080008000ul;}
        if(op<6)r.b[0]=counts[pair%10];
        switch(op) {
        case 0:OP("psllw");break;case 1:OP("pslld");break;case 2:OP("psrlw");break;case 3:OP("psrld");break;case 4:OP("psraw");break;case 5:OP("psrad");break;
        case 6:SHUFFLE("shufps");break;case 7:SHUFFLE("shufpd");break;case 8:SHUFFLE("pshuflw");break;case 9:SHUFFLE("pshufhw");break;
        case 13:OP("andps");break;case 14:OP("andpd");break;case 15:OP("andnps");break;case 16:OP("andnpd");break;case 17:OP("orps");break;case 18:OP("orpd");break;
        case 19:OP("pmullw");break;
        case 20:OP("pmulhw");break;
        case 21:OP("pmulhuw");break;
        case 22:OP("pmuludq");break;
        case 23:OP("pmaddwd");break;
        case 24:OP("psadbw");break;
        case 25:OP("pavgb");break;
        case 26:OP("pavgw");break;
        case 27:OP("paddsb");break;
        case 28:OP("paddsw");break;
        case 29:OP("paddusb");break;
        case 30:OP("paddusw");break;
        case 31:OP("psubsb");break;
        case 32:OP("psubsw");break;
        case 33:OP("psubusb");break;
        case 34:OP("psubusw");break;
        case 35:OP("pminsw");break;
        case 36:OP("pmaxsw");break;
        case 37:OP("unpcklps");break;
        case 38:OP("unpckhps");break;
        case 39:OP("unpcklpd");break;
        case 40:OP("unpckhpd");break;
        case 41:__asm__ volatile(SETUP "movmskps %%xmm1,%%eax; movd %%eax,%%xmm0" FINISH ARGS,"rax");break;
        case 42:__asm__ volatile(SETUP "movmskpd %%xmm1,%%eax; movd %%eax,%%xmm0" FINISH ARGS,"rax");break;
        case 43:SELECT(insert);break;case 44:SELECT(extract);break;
        case 45:__asm__ volatile(SETUP "movntdq %%xmm1,%0; sfence; movdqu %0,%%xmm0" FINISH ARGS);break;
        case 46:__asm__ volatile(SETUP "movntps %%xmm1,%0; sfence; movdqu %0,%%xmm0" FINISH ARGS);break;
        case 47:__asm__ volatile(SETUP "movntpd %%xmm1,%0; sfence; movdqu %0,%%xmm0" FINISH ARGS);break;
        case 10:OP("packsswb");break;case 11:OP("packssdw");break;case 12:OP("packuswb");break;
        }
        r.flags&=0x8d5;
        if(call(1,1,(long)&r,sizeof r)!=sizeof r){call(231,1,0,0);__builtin_unreachable();}
    }

    for(unsigned op=0;op<62;op++) for(unsigned variant=0;variant<8;variant++) for(unsigned pair=0;pair<64;pair++) {
        unsigned long seed=(pair+1)*0x9e3779b97f4a7c15ul;
        struct record r={0};r.op=op+48;r.variant=variant;
        seed ^= seed>>12;seed ^= seed<<25;seed ^= seed>>27;r.a[0]=pair<6?edges[pair]:seed*0x2545f4914f6cdd1dul;
        seed ^= seed>>12;seed ^= seed<<25;seed ^= seed>>27;r.b[0]=pair<6?edges[(pair+3)%6]:seed*0x2545f4914f6cdd1dul;
        if(op>=47 && op<=54)r.b[0]=counts[pair%10];
        if(op==33 && pair==0)r.a[0]=r.b[0]=0x8000800080008000ul;
        switch(op) {
        case 0:MMOP("packsswb");break;
        case 1:MMOP("packssdw");break;
        case 2:MMOP("packuswb");break;
        case 3:MMOP("paddb");break;
        case 4:MMOP("paddw");break;
        case 5:MMOP("paddd");break;
        case 6:MMOP("paddq");break;
        case 7:MMOP("paddsb");break;
        case 8:MMOP("paddsw");break;
        case 9:MMOP("paddusb");break;
        case 10:MMOP("paddusw");break;
        case 11:MMOP("psubb");break;
        case 12:MMOP("psubw");break;
        case 13:MMOP("psubd");break;
        case 14:MMOP("psubq");break;
        case 15:MMOP("psubsb");break;
        case 16:MMOP("psubsw");break;
        case 17:MMOP("psubusb");break;
        case 18:MMOP("psubusw");break;
        case 19:MMOP("pand");break;
        case 20:MMOP("pandn");break;
        case 21:MMOP("por");break;
        case 22:MMOP("pxor");break;
        case 23:MMOP("pcmpeqb");break;
        case 24:MMOP("pcmpeqw");break;
        case 25:MMOP("pcmpeqd");break;
        case 26:MMOP("pcmpgtb");break;
        case 27:MMOP("pcmpgtw");break;
        case 28:MMOP("pcmpgtd");break;
        case 29:MMOP("pmullw");break;
        case 30:MMOP("pmulhw");break;
        case 31:MMOP("pmulhuw");break;
        case 32:MMOP("pmuludq");break;
        case 33:MMOP("pmaddwd");break;
        case 34:MMOP("psadbw");break;
        case 35:MMOP("pavgb");break;
        case 36:MMOP("pavgw");break;
        case 37:MMOP("pminub");break;
        case 38:MMOP("pmaxub");break;
        case 39:MMOP("pminsw");break;
        case 40:MMOP("pmaxsw");break;
        case 41:MMOP("punpcklbw");break;
        case 42:MMOP("punpckhbw");break;
        case 43:MMOP("punpcklwd");break;
        case 44:MMOP("punpckhwd");break;
        case 45:MMOP("punpckldq");break;
        case 46:MMOP("punpckhdq");break;
        case 47:MMOP("psllw");break;
        case 48:MMOP("pslld");break;
        case 49:MMOP("psllq");break;
        case 50:MMOP("psrlw");break;
        case 51:MMOP("psrld");break;
        case 52:MMOP("psrlq");break;
        case 53:MMOP("psraw");break;
        case 54:MMOP("psrad");break;
        case 55:SELECT(mminsert);break;
        case 56:SELECT(mmextract);break;
        case 57:SELECT(mmshuffle);break;
        case 58:__asm__ volatile(MMSETUP "pmovmskb %%mm1,%%eax; movd %%eax,%%mm0" MMFINISH MMARGS,"rax");break;
        case 59:__asm__ volatile(MMSETUP "movntq %%mm1,%0; sfence; movq %0,%%mm0" MMFINISH MMARGS);break;
        case 60:__asm__ volatile(MMSETUP "movq2dq %%mm1,%%xmm0; movq %%xmm0,%0; emms; pushfq; popq %1" MMARGS,"xmm0");break;
        case 61:__asm__ volatile(MMSETUP "movdqu %3,%%xmm0; movdq2q %%xmm0,%%mm0" MMFINISH MMARGS,"xmm0");break;
        }
        r.flags&=0x8d5;
        if(call(1,1,(long)&r,sizeof r)!=sizeof r){call(231,1,0,0);__builtin_unreachable();}
    }
    for(unsigned op=0;op<2;op++) for(unsigned pair=0;pair<512;pair++) {
        struct record r={0};r.op=200+op;r.variant=pair;
        r.a[0]=0x0123456789abcdeful;r.a[1]=~r.a[0];
        r.b[0]=(pair%8==0)?0:(pair%8==1)?~0ul:pair*0x9e3779b97f4a7c15ul;
        r.b[1]=~r.b[0];
        if(op==0) __asm__ volatile(SETUP "maskmovdqu %%xmm1,%%xmm0; sfence; pushfq; popq %1"
            :"=m"(r.result),"=r"(r.flags):"m"(r.a),"m"(r.b),"D"(r.result):"xmm0","xmm1","cc","memory");
        else __asm__ volatile(MMSETUP "maskmovq %%mm1,%%mm0; sfence; emms; pushfq; popq %1"
            :"=m"(r.result),"=r"(r.flags):"m"(r.a),"m"(r.b),"D"(r.result):"mm0","mm1","cc","memory");
        r.flags&=0x8d5;
        if(call(1,1,(long)&r,sizeof r)!=sizeof r){call(231,1,0,0);__builtin_unreachable();}
    }
    call(231,0,0,0);__builtin_unreachable();
}
