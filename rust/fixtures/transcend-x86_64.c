/* Approximate x87 transcendental oracle with explicit input and output extended values. */
static long call(long n,long a,long b,long c) {long r;__asm__ volatile("syscall":"=a"(r):"a"(n),"D"(a),"S"(b),"d"(c):"rcx","r11","memory");return r;}
struct extended {unsigned long fraction;unsigned short exponent;} __attribute__((packed));
static const struct extended extended[]={
{0,0},{0,0x8000},{0x8000000000000000ul,0x3fff},{0x8000000000000000ul,0xbfff},
{0xaaaaaaaaaaaaaaaaul,0x3ffd},{0x8000000000000000ul,0x7fff},{0x8000000000000000ul,0xffff},
{0xc000000000000123ul,0x7fff},{0x8000000000000123ul,0xffff},{1,0},{0x8000000000000000ul,1},
{0xfffffffffffffffful,0x7ffe},{0x8000000000000000ul,0x3fbf},{1,1},{0,0x7fff},{0x7ffffffffffffffful,0x3fff},{0x8000000000000000ul,0},{0xfffffffffffffffful,0},{0x7ffffffffffffffful,0},{0,0x4000},{0,0xffff}};

struct record {struct extended x,y,result;unsigned short status,control,padding;unsigned sample;};
__attribute__((noreturn)) void _start(void) {
    for(unsigned op=0;op<10;op++) for(unsigned mode=0;mode<24;mode++) for(unsigned pair=0;pair<441+2048;pair++) {
        struct record r={0};r.sample=pair;r.padding=op;r.control=0x7f|((mode%12<4?0:mode%12<8?2:3)<<8)|((mode&3)<<10);if(mode>=12)r.control&=~24;
        if(pair<441){r.x=extended[pair%21];r.y=extended[pair/21];}
        else {
            unsigned long seed=(pair-440)*0x9e3779b97f4a7c15ul;
            seed ^= seed>>12;seed ^= seed<<25;seed ^= seed>>27;r.x.fraction=seed*0x2545f4914f6cdd1dul|0x8000000000000000ul;
            seed ^= seed>>12;seed ^= seed<<25;seed ^= seed>>27;r.y.fraction=seed*0x2545f4914f6cdd1dul|0x8000000000000000ul;
            if(pair<441+1024){r.x.exponent=(r.y.fraction>>32)&0xffff;r.y.exponent=(r.x.fraction>>32)&0xffff;}
            else {r.x.exponent=(0x3ffd+(pair&3))|((pair&4)?0x8000:0);r.y.exponent=(0x3ffd+((pair>>3)&3))|((pair&32)?0x8000:0);}
        }
        if(op>=4 && pair<12){
            static const struct extended angles[]={
                {0xc90fdaa22168c234ul,0x3fff},{0xc90fdaa22168c235ul,0x3fff},{0xc90fdaa22168c236ul,0x3fff},
                {0xc90fdaa22168c234ul,0x4000},{0xc90fdaa22168c235ul,0x4000},{0xc90fdaa22168c236ul,0x4000},
                {0xc90fdaa22168c235ul,0x4001},{0x8000000000000000ul,0x403e},{0xfffffffffffffffful,0x403d},
                {0x8000000000000000ul,0x403d},{0x8000000000000000ul,0xc03e},{0xfffffffffffffffful,0xc03d}};
            r.x=angles[pair];
        }
        if(op==3 && (r.x.exponent&0x7fff)!=0x7fff && (r.x.exponent&0x7fff)>0x3ffc)r.x.exponent=(r.x.exponent&0x8000)|0x3ffc;
        if(op>=4){
            struct extended scratch;
            if(op==4)__asm__ volatile("fninit; fldcw %3; fldt %2; fsin; fnstsw %1; fnclex; fstpt %0":"=m"(r.result),"=m"(r.status):"m"(r.x),"m"(r.control):"st","memory");
            if(op==5)__asm__ volatile("fninit; fldcw %3; fldt %2; fcos; fnstsw %1; fnclex; fstpt %0":"=m"(r.result),"=m"(r.status):"m"(r.x),"m"(r.control):"st","memory");
            if(op==6)__asm__ volatile("fninit; fldcw %4; fldt %3; fptan; fnstsw %1; fnclex; fstpt %2; fstpt %0":"=m"(r.result),"=m"(r.status),"=m"(scratch):"m"(r.x),"m"(r.control):"st","st(1)","memory");
            if(op==7)__asm__ volatile("fninit; fldcw %3; fldt %2; fsincos; fnstsw %1; fnclex; fstpt %0":"=m"(r.result),"=m"(r.status):"m"(r.x),"m"(r.control):"st","st(1)","memory");
            if(op==8)__asm__ volatile("fninit; fldcw %4; fldt %3; fsincos; fnstsw %1; fnclex; fstpt %2; fstpt %0":"=m"(r.result),"=m"(r.status),"=m"(scratch):"m"(r.x),"m"(r.control):"st","st(1)","memory");
            if(op==9)__asm__ volatile("fninit; fldcw %3; fldt %2; fptan; fnstsw %1; fnclex; fstpt %0":"=m"(r.result),"=m"(r.status):"m"(r.x),"m"(r.control):"st","st(1)","memory");
        }else if(op==1){
            unsigned exponent=r.x.exponent&0x7fff;
            if(exponent!=0x7fff && (exponent>0x3fff || (exponent==0x3fff && r.x.fraction>0x8000000000000000ul)))r.x.exponent=(r.x.exponent&0x8000)|0x3ffe;
            __asm__ volatile("fninit; fldcw %3; fldt %2; f2xm1; fnstsw %1; fnclex; fstpt %0":"=m"(r.result),"=m"(r.status):"m"(r.x),"m"(r.control):"st","memory");
        }else if(op==2){
            __asm__ volatile("fninit; fldcw %4; fldt %2; fldt %3; fyl2x; fnstsw %1; fnclex; fstpt %0":"=m"(r.result),"=m"(r.status):"m"(r.y),"m"(r.x),"m"(r.control):"st","st(1)","memory");
        }else if(op==3){
            __asm__ volatile("fninit; fldcw %4; fldt %2; fldt %3; fyl2xp1; fnstsw %1; fnclex; fstpt %0":"=m"(r.result),"=m"(r.status):"m"(r.y),"m"(r.x),"m"(r.control):"st","st(1)","memory");
        }else{
            __asm__ volatile("fninit; fldcw %4; fldt %2; fldt %3; fpatan; fnstsw %1; fnclex; fstpt %0":"=m"(r.result),"=m"(r.status):"m"(r.y),"m"(r.x),"m"(r.control):"st","st(1)","memory");
        }
        if(call(1,1,(long)&r,sizeof r)!=sizeof r){call(231,1,0,0);__builtin_unreachable();}
    }
    call(231,0,0,0);__builtin_unreachable();
}
