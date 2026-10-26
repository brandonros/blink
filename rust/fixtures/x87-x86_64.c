/* Native x87 result and status oracle; explicit ten-byte extended values. */
static long call(long n,long a,long b,long c) {long r;__asm__ volatile("syscall":"=a"(r):"a"(n),"D"(a),"S"(b),"d"(c):"rcx","r11","memory");return r;}
static const unsigned long values[]={0,0x8000000000000000ul,0x3ff0000000000000ul,0xbff0000000000000ul,0x3fd5555555555555ul,0x7ff0000000000000ul,0xfff0000000000000ul,0x7ff8000000000123ul,0xfff0000000000123ul,1,0x0010000000000000ul,0x7feffffffffffffful,0x3ca0000000000000ul,0,0,0,0,0,0,0,0};
struct extended {unsigned long fraction;unsigned short exponent;} __attribute__((packed));
static const struct extended extended[]={
{0,0},{0,0x8000},{0x8000000000000000ul,0x3fff},{0x8000000000000000ul,0xbfff},
{0xaaaaaaaaaaaaaaaaul,0x3ffd},{0x8000000000000000ul,0x7fff},{0x8000000000000000ul,0xffff},
{0xc000000000000123ul,0x7fff},{0x8000000000000123ul,0xffff},{1,0},{0x8000000000000000ul,1},
{0xfffffffffffffffful,0x7ffe},{0x8000000000000000ul,0x3fbf},{1,1},{0,0x7fff},{0x7ffffffffffffffful,0x3fff},{0x8000000000000000ul,0},{0xfffffffffffffffful,0},{0x7ffffffffffffffful,0},{0,0x4000},{0,0xffff}};
struct record {unsigned long a,b;unsigned char result[10];unsigned short status,control,op;};
#define OPERATE(instruction) __asm__ volatile("fninit; fldcw %4; fldl %2; fldl %3; " instruction "; fnstsw %1; fstpt %0; fninit" :"=m"(r.result),"=m"(r.status):"m"(r.a),"m"(r.b),"m"(r.control):"st","st(1)","rax","cc","memory")
#define OPERATE80(instruction) __asm__ volatile("fninit; fldcw %4; fldt %2; fldt %3; " instruction "; fnstsw %1; fstpt %0; fninit" :"=m"(r.result),"=m"(r.status):"m"(left),"m"(right),"m"(r.control):"st","st(1)","rax","cc","memory")
#define STORE80(instruction) __asm__ volatile("fninit; fldcw %4; fldt %2; fldt %3; " instruction " %0; fnstsw %1; fninit" :"=m"(r.result),"=m"(r.status):"m"(left),"m"(right),"m"(r.control):"st","st(1)","rax","cc","memory")
#define COMPARE80(instruction) do { __asm__ volatile("fninit; fldcw %4; fldt %2; fldt %3; " instruction "; pushfq; popq %0; fnstsw %1; fninit" :"=m"(r.result),"=m"(r.status):"m"(left),"m"(right),"m"(r.control):"st","st(1)","memory","cc"); r.result[0]&=0xd5;r.result[1]&=8;for(int n=2;n<10;n++)r.result[n]=0; } while(0)
__attribute__((noreturn)) void _start(void) {
    for(unsigned op=0;op<26;op++) for(unsigned mode=0;mode<12;mode++) for(unsigned pair=0;pair<441+256;pair++) {
        struct record r={0};r.op=op;r.control=0x7f|((mode<4?0:mode<8?2:3)<<8)|((mode&3)<<10);
        if(pair<441) {r.a=values[pair/21];r.b=values[pair%21];}
        else {
            unsigned long seed=(pair-440)*0x9e3779b97f4a7c15ul;
            seed ^= seed>>12; seed ^= seed<<25; seed ^= seed>>27;r.a=seed*0x2545f4914f6cdd1dul;
            seed ^= seed>>12; seed ^= seed<<25; seed ^= seed>>27;r.b=seed*0x2545f4914f6cdd1dul;
        }
        struct extended left,right;
        if(pair<441) {left=extended[pair/21];right=extended[pair%21];}
        else {left.fraction=r.a|0x8000000000000000ul;left.exponent=(r.b>>32)&0xffff;right.fraction=r.b|0x8000000000000000ul;right.exponent=(r.a>>32)&0xffff;}
        switch(op) {
        case 8:OPERATE80("fcom %%st(1)");break;
        case 9:OPERATE80("fucompp");break;
        case 10:COMPARE80("fcomip %%st(1)");break;
        case 11:COMPARE80("fucomip %%st(1)");break;
        case 12:OPERATE80("frndint");break;
        case 13:STORE80("fstps");break;
        case 14:STORE80("fstpl");break;
        case 15:STORE80("fistpq");break;
        case 16:STORE80("fistpl");break;
        case 17:STORE80("fistps");break;
        case 21:OPERATE80("fxtract");break;
        case 22:OPERATE80("fxtract; fxch %%st(1)");break;
        case 23:OPERATE80("1: fprem; fnstsw %%ax; testw $0x400,%%ax; jnz 1b");break;
        case 24:OPERATE80("1: fprem1; fnstsw %%ax; testw $0x400,%%ax; jnz 1b");break;
        case 25:STORE80("fbstp");break;
        case 20:OPERATE80("fscale");break;
        case 19:OPERATE80("fsqrt");break;
        case 18:STORE80("fisttpq");break;
        case 4:OPERATE80("faddp");break;case 5:OPERATE80("fsubp");break;case 6:OPERATE80("fmulp");break;case 7:OPERATE80("fdivp");break;case 0:OPERATE("faddp");break;case 1:OPERATE("fsubp");break;case 2:OPERATE("fmulp");break;case 3:OPERATE("fdivp");break;}
        if(call(1,1,(long)&r,sizeof r)!=sizeof r) {call(231,1,0,0);__builtin_unreachable();}
    }
    /* Midpoints plus a tiny term expose double rounding through 64/111 bits. */
    for(unsigned mode=0;mode<12;mode++) for(unsigned precision=0;precision<2;precision++) for(unsigned op=0;op<2;op++) {
        struct extended left={0x8000000000000000ul | (1ul<<(precision?10:39)),0x3fff};
        struct extended right={0x8000000000000000ul,0x3fff-1000};
        struct record r={0};r.a=left.fraction;r.b=right.exponent;r.op=50+op;
        r.control=0x7f|((mode<4?0:mode<8?2:3)<<8)|((mode&3)<<10);
        if(op==0) {OPERATE80("faddp");} else {OPERATE80("fsubp");}
        if(call(1,1,(long)&r,sizeof r)!=sizeof r) {call(231,1,0,0);__builtin_unreachable();}
    }
    /* Square-root encodings, extreme exponents and full-width random inputs. */
    static const struct extended special[]={
        {1,1},{0,0x7fff},{0x8000000000000000ul,0},
        {0xfffffffffffffffful,0},{1,0},{0x7ffffffffffffffful,0},
        {0x8000000000000001ul,0x3fff},{0xfffffffffffffffful,0x7ffe}
    };
    for(unsigned mode=0;mode<12;mode++) for(unsigned sample=0;sample<4104;sample++) {
        struct extended left={0,0},right;
        if(sample<8)right=special[sample];
        else {unsigned long seed=sample*0x9e3779b97f4a7c15ul;seed^=seed>>12;seed^=seed<<25;seed^=seed>>27;right.fraction=seed|0x8000000000000000ul;right.exponent=(seed>>32)&0x7fff;}
        struct record r={0};r.a=right.fraction;r.b=right.exponent;r.op=52;
        r.control=0x7f|((mode<4?0:mode<8?2:3)<<8)|((mode&3)<<10);
        OPERATE80("fsqrt");
        if(call(1,1,(long)&r,sizeof r)!=sizeof r) {call(231,1,0,0);__builtin_unreachable();}
    }
    for(unsigned mode=0;mode<12;mode++) for(unsigned op=0;op<5;op++) {
        struct extended left={0,0},right={0,0};
        struct record r={0};r.op=53+op;
        r.control=0x7f|((mode<4?0:mode<8?2:3)<<8)|((mode&3)<<10);
        switch(op){case 0:OPERATE80("fldl2t");break;case 1:OPERATE80("fldl2e");break;case 2:OPERATE80("fldpi");break;case 3:OPERATE80("fldlg2");break;case 4:OPERATE80("fldln2");break;}
        if(call(1,1,(long)&r,sizeof r)!=sizeof r) {call(231,1,0,0);__builtin_unreachable();}
    }
    for(unsigned sample=0;sample<21;sample++) for(unsigned empty=0;empty<2;empty++) {
        struct extended left={0,0},right=sample<13?extended[sample]:special[sample-13];
        struct record r={0};r.a=right.fraction;r.b=right.exponent;r.op=58+empty;r.control=0x37f;
        if(empty){OPERATE80("ffree %%st(0); fxam");}else{OPERATE80("fxam");}
        if(call(1,1,(long)&r,sizeof r)!=sizeof r) {call(231,1,0,0);__builtin_unreachable();}
    }
    for(unsigned sign=0;sign<2;sign++) for(unsigned sample=0;sample<1024;sample++) {
        unsigned long number=(sample*0x9e3779b97f4a7c15ul)%1000000000000000000ul;
        if(sample==0)number=0;if(sample==1)number=999999999999999999ul;
        unsigned char bcd[10]={0};unsigned long remaining=number;
        for(int n=0;n<9;n++){unsigned lo=remaining%10;remaining/=10;unsigned hi=remaining%10;remaining/=10;bcd[n]=lo|(hi<<4);}bcd[9]=sign?128:0;
        struct record r={0};r.a=number;r.b=sign;r.op=60;r.control=0x37f;
        __asm__ volatile("fninit; fbld %2; fnstsw %1; fstpt %0":"=m"(r.result),"=m"(r.status):"m"(bcd):"st","memory");
        if(call(1,1,(long)&r,sizeof r)!=sizeof r) {call(231,1,0,0);__builtin_unreachable();}
    }
    static const unsigned long integers[]={0,1,-1ul,32767,32768,0x7fffffff,0x80000000,0xabcdef1234567890};
    for(unsigned op=0;op<4;op++) for(unsigned mode=0;mode<12;mode++) for(unsigned sample=0;sample<13;sample++) for(unsigned i=0;i<8;i++) {
        struct record r={0};r.op=61+op;r.a=integers[i];r.b=sample;
        r.control=0x7f|((mode<4?0:mode<8?2:3)<<8)|((mode&3)<<10);
        struct extended value=extended[sample];
#define ICOMPARE(ins) __asm__ volatile("fninit; fldcw %4; fldz; fldt %3; " ins " %2; fnstsw %1; fstpt %0":"=m"(r.result),"=m"(r.status):"m"(r.a),"m"(value),"m"(r.control):"st","st(1)","memory")
        switch(op){case 0:ICOMPARE("ficoms");break;case 1:ICOMPARE("ficoml");break;case 2:ICOMPARE("ficomps");break;case 3:ICOMPARE("ficompl");break;}
        if(call(1,1,(long)&r,sizeof r)!=sizeof r) {call(231,1,0,0);__builtin_unreachable();}
    }
    for(unsigned mode=0;mode<12;mode++) for(unsigned sign=0;sign<2;sign++) {
        struct extended left={0xfffffffffffffffful,sign?0x8000:0},right={0x8000000000000000ul,0x3ffe};
        struct record r={0};r.a=left.fraction;r.b=left.exponent;r.op=65;
        r.control=0x7f|((mode<4?0:mode<8?2:3)<<8)|((mode&3)<<10);
        OPERATE80("fmulp");
        if(call(1,1,(long)&r,sizeof r)!=sizeof r) {call(231,1,0,0);__builtin_unreachable();}
    }
    call(231,0,0,0);__builtin_unreachable();
}
