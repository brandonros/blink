/* Conditional x87 moves, including empty stack slots and non-numeric encodings. */
static long call(long n,long a,long b,long c) {long r;__asm__ volatile("syscall":"=a"(r):"a"(n),"D"(a),"S"(b),"d"(c):"rcx","r11","memory");return r;}
static const unsigned long values[][2]={{0,0},{0x8000000000000000ul,0x3fff},{0xc000000000000000ul,0x4000},{0xc000000000000123ul,0x7fff},{0x8000000000000123ul,0x7fff},{1,0}};
struct record {unsigned long low;unsigned short high,status;unsigned char tag,padding[3];unsigned long flags;unsigned op,variant;};
static unsigned char state[512] __attribute__((aligned(16)));
#define OP(instruction) __asm__ volatile("pushq %2; popfq; " instruction " %%st(1),%%st; pushfq; popq %0; fxsave64 %1":"=r"(r.flags),"=m"(state):"r"(flags):"st","cc","memory")
#define UNARY(instruction) __asm__ volatile("pushq %2; popfq; " instruction "; pushfq; popq %0; fxsave64 %1":"=r"(r.flags),"=m"(state):"r"(flags):"st","cc","memory")
__attribute__((noreturn,force_align_arg_pointer)) void _start(void) {
    for(unsigned op=0;op<21;op++)for(unsigned flags_index=0;flags_index<32;flags_index++)for(unsigned variant=0;variant<9;variant++) {
        struct record r={0};r.op=op;r.variant=variant;
        __asm__ volatile("fninit; fldt %0; fldt %1"::"m"(values[variant%6]),"m"(values[(variant+3)%6]):"st","st(1)","memory");
        if(variant==6||variant==8)__asm__ volatile("ffree %%st(1)":::"memory");
        if(variant==7||variant==8)__asm__ volatile("ffree %%st(0)":::"memory");
        unsigned long flags=2|(flags_index&1)|((flags_index&2)?4:0)|((flags_index&4)?64:0)|((flags_index&8)?128:0)|((flags_index&16)?2048:0);
        switch(op){case 0:OP("fcmovb");break;case 1:OP("fcmovbe");break;case 2:OP("fcmove");break;case 3:OP("fcmovnb");break;case 4:OP("fcmovnbe");break;case 5:OP("fcmovne");break;case 6:OP("fcmovnu");break;case 7:OP("fcmovu");break;case 8:UNARY("fchs");break;case 9:UNARY("fabs");break;
        case 10:OP("fadd");break;case 11:OP("fmul");break;case 12:OP("fsub");break;case 13:OP("fdiv");break;
        case 14:UNARY("fxch %%st(1)");break;case 15:UNARY("frndint");break;case 16:UNARY("fsqrt");break;case 17:UNARY("fscale");break;case 18:UNARY("fxtract");break;case 19:UNARY("fld1; fld1; fld1; fld1; fld1; fld1; fxtract");break;case 20:UNARY("fld1; fld1; fld1; fld1; fld1; fld1; fxtract; fxch %%st(1)");break;}
        for(int n=0;n<8;n++)r.low|=(unsigned long)state[32+n]<<(n*8);
        r.high=state[40]|(state[41]<<8);r.status=state[2]|(state[3]<<8);r.tag=state[4];r.flags&=0x8d5;
        if(call(1,1,(long)&r,sizeof r)!=sizeof r){call(231,1,0,0);__builtin_unreachable();}
    }
    call(231,0,0,0);__builtin_unreachable();
}
