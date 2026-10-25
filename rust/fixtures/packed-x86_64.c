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
    call(231,0,0,0);__builtin_unreachable();
}
